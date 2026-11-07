//! egui/eframe front end; reads `StateHandle`, sends `Command`s.
//!
//! The UI holds no business logic and performs no I/O (RULES.md §7): every
//! user action is a [`Command`] pushed into `vrc-core`, and every frame is
//! rendered from an [`AppState`] snapshot. Passwords/2FA codes live only in
//! local form buffers, are moved into `SecretString`s on submit, and are
//! never stored in `AppState` or logged (RULES.md §4).

mod browse;
mod social;
mod tabs;

use std::collections::HashSet;

use eframe::egui;
use secrecy::SecretString;
use tokio::sync::broadcast;
use tokio::sync::mpsc::error::TrySendError;
use vrc_core::{
    search, sections, AppHandle, AppState, AuthState, Command, CommandSender, Friend,
    GroupAccessType, InstanceKind, InstanceRegion, PipelineStatus, StateHandle, TimelineKind,
    TrustRank, TwoFactorMethod,
};

/// Which worlds feed the Worlds tab lists (`GET /worlds`, `/worlds/active`
/// or `/worlds/recent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorldsFeed {
    /// `GET /worlds` — free-text search/browse.
    Search,
    /// `GET /worlds/active` — currently active worlds.
    Active,
    /// `GET /worlds/recent` — recently visited worlds.
    Recent,
}

/// A destructive action awaiting explicit user confirmation (RULES.md:
/// unfriend/block never fire without one).
#[derive(Debug, Clone)]
pub(crate) enum ConfirmAction {
    /// `Command::Unfriend { confirm: true }`.
    Unfriend {
        /// `usr_…` target id.
        user_id: String,
    },
    /// `Command::ModerateUser { kind: Block, confirm: true }`.
    Block {
        /// `usr_…` target id.
        user_id: String,
    },
    /// `Command::ClearNotifications` — destructive, needs the dialog.
    ClearNotifications,
    /// `Command::SelectAvatar { confirm: true }` — avatar switch.
    SelectAvatar {
        /// `avtr_…` avatar id.
        avatar_id: String,
        /// Display name shown in the dialog.
        name: String,
    },
}

/// Root eframe application.
pub struct VrcNextApp {
    commands: CommandSender,
    state: StateHandle,
    /// Login form buffers — moved into `SecretString`s on submit, never
    /// persisted or logged.
    username: String,
    password: String,
    code: String,
    /// Index into the `Needs2fa` method list.
    twofa_index: usize,
    /// Live friend search query.
    search: String,
    /// Active central-panel tab.
    tab: tabs::Tab,
    /// Timeline tab filter state.
    timeline_kind: Option<TimelineKind>,
    timeline_query: String,
    timeline_page: u32,
    /// Friend-history tab filter state (`history_user` = one `usr_…` id).
    history_user: Option<String>,
    history_query: String,
    history_page: u32,
    /// Stats tab range preset.
    stats_preset: tabs::StatsPreset,
    /// Profile-tab note editor buffer, keyed by the profile's user id.
    note_edit: String,
    /// Which user's note `note_edit` currently edits.
    note_edit_user: Option<String>,
    /// Whether the multi-invite picker window is open.
    multi_invite_open: bool,
    /// Friend ids ticked in the multi-invite picker.
    multi_invite_selected: HashSet<String>,
    /// Pending destructive action shown in the confirm dialog.
    confirm: Option<ConfirmAction>,
    /// Worlds-tab live search query (applies to every worlds feed).
    world_query: String,
    /// Worlds-tab feed selector.
    worlds_feed: WorldsFeed,
    /// Groups-tab mode: `true` = own groups, `false` = search.
    groups_mine: bool,
    /// Groups-tab live search query.
    group_query: String,
    /// Avatars-tab scope: `true` = own avatars, `false` = featured.
    avatar_own: bool,
    /// Open world-detail id on the worlds tab.
    world_detail: Option<String>,
    /// Open group-detail id on the groups tab.
    group_detail: Option<String>,
    /// Open avatar-detail id on the avatars tab.
    avatar_detail: Option<String>,
    /// Instance kind for the world-detail create-instance picker.
    new_instance_kind: InstanceKind,
    /// Instance region for the world-detail create-instance picker.
    new_instance_region: InstanceRegion,
    /// `grp_…` owner id for a group instance (kind = Group).
    new_instance_group: String,
    /// Group access type for a group instance (kind = Group).
    new_instance_access: GroupAccessType,
    /// `canRequestInvite` for a private instance (Invite+ vs Invite).
    new_instance_can_request_invite: bool,
}

impl VrcNextApp {
    fn new(commands: CommandSender, state: StateHandle) -> Self {
        let app = Self {
            commands,
            state,
            username: String::new(),
            password: String::new(),
            code: String::new(),
            twofa_index: 0,
            search: String::new(),
            tab: tabs::Tab::Dashboard,
            timeline_kind: None,
            timeline_query: String::new(),
            timeline_page: 0,
            history_user: None,
            history_query: String::new(),
            history_page: 0,
            stats_preset: tabs::StatsPreset::AllTime,
            note_edit: String::new(),
            note_edit_user: None,
            multi_invite_open: false,
            multi_invite_selected: HashSet::new(),
            confirm: None,
            world_query: String::new(),
            worlds_feed: WorldsFeed::Search,
            groups_mine: false,
            group_query: String::new(),
            avatar_own: false,
            world_detail: None,
            group_detail: None,
            avatar_detail: None,
            new_instance_kind: InstanceKind::Public,
            new_instance_region: InstanceRegion::Us,
            new_instance_group: String::new(),
            new_instance_access: GroupAccessType::Members,
            new_instance_can_request_invite: false,
        };
        // Resume a persisted session on startup. With an empty vault this
        // resolves to `LoggedOut` without any network call (RULES.md §4).
        app.send(Command::ResumeSession);
        app
    }

    /// Queue a command; the channel is bounded and commands are cheap, so a
    /// full channel simply drops the action rather than blocking the UI.
    pub(crate) fn send(&self, command: Command) {
        if let Err(TrySendError::Closed(_)) = self.commands.try_send(command) {
            tracing::warn!("vrc-core command channel closed");
        }
    }

    fn login_ui(&mut self, ui: &mut egui::Ui, state: &AppState) {
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("VRCNextRust");
                ui.add_space(16.0);
                ui.set_max_width(320.0);

                let user_out = egui::TextEdit::singleline(&mut self.username)
                    .hint_text("Username or e-mail")
                    .desired_width(f32::INFINITY)
                    .show(ui);
                let pass_out = egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .hint_text("Password")
                    .desired_width(f32::INFINITY)
                    .show(ui);
                ui.add_space(8.0);

                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let submit = ui.button("Log in").clicked()
                    || (enter && (user_out.response.has_focus() || pass_out.response.has_focus()));
                if submit && !self.username.trim().is_empty() && !self.password.is_empty() {
                    // Move both buffers out of UI state: the password becomes
                    // a SecretString (zeroized when the command is dropped),
                    // and the username (typically an e-mail) is cleared —
                    // credentials live in memory only for the login call
                    // (RULES.md §4).
                    let username = std::mem::take(&mut self.username);
                    let password = SecretString::from(std::mem::take(&mut self.password));
                    self.send(Command::Login {
                        username: username.trim().to_string(),
                        password,
                    });
                }

                error_text(ui, state);
                ui.add_space(16.0);
                if ui
                    .button("Resume saved session")
                    .on_hover_text("Uses the cookie stored in the OS credential store")
                    .clicked()
                {
                    self.send(Command::ResumeSession);
                }
            });
        });
    }

    fn twofa_ui(&mut self, ui: &mut egui::Ui, methods: &[TwoFactorMethod], state: &AppState) {
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("Two-factor authentication");
                ui.add_space(16.0);
                ui.set_max_width(320.0);

                if methods.len() > 1 {
                    self.twofa_index = self.twofa_index.min(methods.len() - 1);
                    let selected = twofa_method_label(methods[self.twofa_index]);
                    egui::ComboBox::from_label("Method")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (i, method) in methods.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.twofa_index,
                                    i,
                                    twofa_method_label(*method),
                                );
                            }
                        });
                    ui.add_space(8.0);
                }

                let out = egui::TextEdit::singleline(&mut self.code)
                    .password(true)
                    .hint_text("One-time code")
                    .desired_width(f32::INFINITY)
                    .show(ui);
                ui.add_space(8.0);

                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Verify").clicked() || (enter && out.response.has_focus()))
                    && !self.code.trim().is_empty()
                    && !methods.is_empty()
                {
                    let method = methods[self.twofa_index.min(methods.len() - 1)];
                    let code = SecretString::from(std::mem::take(&mut self.code));
                    self.send(Command::Submit2fa { method, code });
                }

                error_text(ui, state);
                ui.add_space(16.0);
                if ui.button("Back to login").clicked() {
                    // No session exists yet; logout clears the pending 2FA
                    // cookie state and returns to the login screen.
                    self.send(Command::Logout);
                }
            });
        });
    }

    fn top_bar(&self, ui: &mut egui::Ui, state: &AppState) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(user) = &state.current_user {
                    ui.strong(&user.display_name);
                    ui.label(format!("<{}>", user.username));
                } else {
                    ui.strong("VRCNextRust");
                }
                ui.separator();
                let (color, label) = pipeline_badge(state.pipeline);
                ui.colored_label(color, format!("● {label}"));
                if let Some(error) = &state.last_error {
                    ui.colored_label(egui::Color32::LIGHT_RED, "error");
                    ui.label(error).on_hover_text(error.clone());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Log out").clicked() {
                        self.send(Command::Logout);
                    }
                    if ui.button("Refresh friends").clicked() {
                        self.send(Command::RefreshFriends);
                    }
                });
            });
        });
    }

    fn friends_panel(&mut self, ui: &mut egui::Ui, state: &AppState) {
        egui::Panel::left("friends_panel")
            .default_size(300.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search friends…")
                    .desired_width(f32::INFINITY)
                    .show(ui);
                ui.separator();
                if ui.button("Multi-invite…").clicked() {
                    self.multi_invite_open = true;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let grouped = search(&sections(state), &self.search);
                    friend_section(self, ui, "Favorites", &grouped.favorites);
                    friend_section(self, ui, "In Game", &grouped.in_game);
                    friend_section(self, ui, "Web / App", &grouped.web_active);
                    friend_section(self, ui, "Offline", &grouped.offline);
                });
            });
    }

    fn main_ui(&mut self, ui: &mut egui::Ui, state: &AppState) {
        self.top_bar(ui, state);
        self.friends_panel(ui, state);
        tabs::central_panel(self, ui, state);
        // Floating windows render above the central panel.
        social::multi_invite_window(self, ui, state);
        social::confirm_dialog(self, ui);
    }
}

impl eframe::App for VrcNextApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let state = self.state.snapshot();
        match &state.auth {
            AuthState::LoggedOut => self.login_ui(ui, &state),
            AuthState::Needs2fa(methods) => self.twofa_ui(ui, methods, &state),
            AuthState::Authenticated => self.main_ui(ui, &state),
        }
    }
}

/// Red error line for the auth screens; cleared by the core on the next
/// command attempt.
fn error_text(ui: &mut egui::Ui, state: &AppState) {
    if let Some(error) = &state.last_error {
        ui.colored_label(egui::Color32::LIGHT_RED, error);
    }
}

/// One collapsible friend section inside the left panel.
fn friend_section(app: &mut VrcNextApp, ui: &mut egui::Ui, title: &str, friends: &[Friend]) {
    egui::CollapsingHeader::new(format!("{title} ({})", friends.len()))
        .default_open(true)
        .show(ui, |ui| {
            if friends.is_empty() {
                ui.weak("—");
                return;
            }
            for friend in friends {
                friend_row(app, ui, friend);
            }
        });
    ui.add_space(4.0);
}

/// A single friend row: presence dot, name (click → profile), trust badge,
/// status/location, and a small invite button.
fn friend_row(app: &mut VrcNextApp, ui: &mut egui::Ui, friend: &Friend) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("●")
                .color(presence_color(friend))
                .size(14.0),
        );
        if ui
            .strong(&friend.display_name)
            .on_hover_text("Open profile")
            .clicked()
        {
            app.send(Command::LoadProfile {
                user_id: friend.id.clone(),
            });
            app.tab = tabs::Tab::Profile;
        }
        let (color, label) = trust_badge(friend.trust_rank);
        ui.colored_label(color, label);
        if friend.is_online()
            && ui
                .small_button("invite")
                .on_hover_text("Invite to your instance")
                .clicked()
        {
            app.send(Command::InviteUser {
                user_id: friend.id.clone(),
                instance_id: None,
                message_slot: None,
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Status description first (the user's own status text), then
            // the location tag, then the presence status as last resort.
            let detail = if !friend.status_description.is_empty() {
                friend.status_description.clone()
            } else if !friend.location.is_empty() {
                friend.location.clone()
            } else {
                friend.status.clone()
            };
            ui.weak(detail);
        });
    });
}

/// Trust rank → display label (low to high, matching `TrustRank` order).
pub fn trust_label(rank: TrustRank) -> &'static str {
    match rank {
        TrustRank::Nuisance => "Nuisance",
        TrustRank::Visitor => "Visitor",
        TrustRank::NewUser => "New User",
        TrustRank::User => "User",
        TrustRank::KnownUser => "Known User",
        TrustRank::TrustedUser => "Trusted User",
        TrustRank::Veteran => "Veteran",
    }
}

/// Trust rank → badge color: warmer/higher trust reads greener.
pub fn trust_color(rank: TrustRank) -> egui::Color32 {
    match rank {
        TrustRank::Nuisance => egui::Color32::LIGHT_RED,
        TrustRank::Visitor => egui::Color32::GRAY,
        TrustRank::NewUser => egui::Color32::from_rgb(0x8a, 0x8a, 0x4a),
        TrustRank::User => egui::Color32::from_rgb(0x4a, 0x7a, 0xc8),
        TrustRank::KnownUser => egui::Color32::from_rgb(0x3f, 0xa7, 0x5d),
        TrustRank::TrustedUser => egui::Color32::from_rgb(0x2e, 0xc4, 0x7e),
        TrustRank::Veteran => egui::Color32::GOLD,
    }
}

/// `(color, label)` pair for a trust badge.
pub fn trust_badge(rank: TrustRank) -> (egui::Color32, &'static str) {
    (trust_color(rank), trust_label(rank))
}

/// Presence dot color: green in-game, blue on web/app, gray offline.
pub fn presence_color(friend: &Friend) -> egui::Color32 {
    match friend.state.as_str() {
        "online" => egui::Color32::from_rgb(0x3f, 0xa7, 0x5d),
        "active" => egui::Color32::from_rgb(0x4a, 0x7a, 0xc8),
        _ => egui::Color32::GRAY,
    }
}

/// Pipeline status → `(color, label)` for the top-bar badge.
/// `Connecting` counts as the "reconnecting" state (it follows a drop or
/// the initial connect).
pub fn pipeline_badge(status: PipelineStatus) -> (egui::Color32, &'static str) {
    match status {
        PipelineStatus::Connected => (egui::Color32::from_rgb(0x3f, 0xa7, 0x5d), "live"),
        PipelineStatus::Connecting => (egui::Color32::GOLD, "reconnecting"),
        PipelineStatus::Disconnected => (egui::Color32::GRAY, "offline"),
    }
}

/// Display label for a 2FA method.
pub fn twofa_method_label(method: TwoFactorMethod) -> &'static str {
    match method {
        TwoFactorMethod::Totp => "Authenticator app (TOTP)",
        TwoFactorMethod::EmailOtp => "E-mail code",
        TwoFactorMethod::RecoveryOtp => "Recovery code",
    }
}

/// Short timestamp for notification rows (shared with the CLI via
/// `vrc_core::format`).
pub(crate) fn fmt_short_ts(iso: &str) -> String {
    vrc_core::format::fmt_iso_ts(iso)
}

/// Open the "VRCNextRust" window and run the UI loop.
///
/// Forwards every [`AppEvent`] to `ctx.request_repaint` from a dedicated
/// thread so live updates repaint without polling.
pub fn run(handle: AppHandle, options: eframe::NativeOptions) -> eframe::Result<()> {
    let AppHandle {
        commands,
        events,
        state,
    } = handle;
    eframe::run_native(
        "VRCNextRust",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            std::thread::Builder::new()
                .name("vrc-ui-events".to_string())
                .spawn(move || {
                    let mut events = events;
                    loop {
                        // `blocking_recv` is fine here: this is a plain
                        // thread, not inside a Tokio runtime.
                        match events.blocking_recv() {
                            Ok(_) => ctx.request_repaint(),
                            // Lagged receivers still get future events;
                            // only a closed channel ends the thread.
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                ctx.request_repaint();
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                })
                .expect("spawn vrc-ui-events thread");
            Ok(Box::new(VrcNextApp::new(commands, state)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_labels_cover_every_rank() {
        let ranks = [
            TrustRank::Nuisance,
            TrustRank::Visitor,
            TrustRank::NewUser,
            TrustRank::User,
            TrustRank::KnownUser,
            TrustRank::TrustedUser,
            TrustRank::Veteran,
        ];
        for rank in ranks {
            let (_, label) = trust_badge(rank);
            assert!(!label.is_empty());
        }
        // Higher trust never renders as the nuisance color.
        assert_ne!(
            trust_color(TrustRank::Veteran),
            trust_color(TrustRank::Nuisance)
        );
    }

    #[test]
    fn presence_color_maps_states() {
        let friend = |state: &str| Friend {
            state: state.to_string(),
            ..Friend::default()
        };
        assert_ne!(presence_color(&friend("online")), egui::Color32::GRAY);
        assert_ne!(presence_color(&friend("active")), egui::Color32::GRAY);
        assert_eq!(presence_color(&friend("offline")), egui::Color32::GRAY);
        assert_eq!(presence_color(&friend("")), egui::Color32::GRAY);
    }

    #[test]
    fn pipeline_badge_maps_all_statuses() {
        assert_eq!(pipeline_badge(PipelineStatus::Connected).1, "live");
        assert_eq!(pipeline_badge(PipelineStatus::Connecting).1, "reconnecting");
        assert_eq!(pipeline_badge(PipelineStatus::Disconnected).1, "offline");
    }

    #[test]
    fn twofa_labels_cover_every_method() {
        for method in [
            TwoFactorMethod::Totp,
            TwoFactorMethod::EmailOtp,
            TwoFactorMethod::RecoveryOtp,
        ] {
            assert!(!twofa_method_label(method).is_empty());
        }
    }
}
