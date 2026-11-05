//! egui/eframe front end; reads `StateHandle`, sends `Command`s.
//!
//! The UI holds no business logic and performs no I/O (RULES.md §7): every
//! user action is a [`Command`] pushed into `vrc-core`, and every frame is
//! rendered from an [`AppState`] snapshot. Passwords/2FA codes live only in
//! local form buffers, are moved into `SecretString`s on submit, and are
//! never stored in `AppState` or logged (RULES.md §4).

use eframe::egui;
use secrecy::SecretString;
use tokio::sync::broadcast;
use tokio::sync::mpsc::error::TrySendError;
use vrc_core::{
    dashboard, search, sections, AppHandle, AppState, AuthState, Command, CommandSender, Friend,
    PipelineStatus, StateHandle, TrustRank, TwoFactorMethod,
};

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
}

impl VrcNextApp {
    fn new(commands: CommandSender, state: StateHandle) -> Self {
        Self {
            commands,
            state,
            username: String::new(),
            password: String::new(),
            code: String::new(),
            twofa_index: 0,
            search: String::new(),
        }
    }

    /// Queue a command; the channel is bounded and commands are cheap, so a
    /// full channel simply drops the action rather than blocking the UI.
    fn send(&self, command: Command) {
        if let Err(TrySendError::Closed(_)) = self.commands.try_send(command) {
            tracing::warn!("vrc-core command channel closed");
        }
    }

    fn login_ui(&mut self, ui: &mut egui::Ui) {
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
                    // Move the buffer into a SecretString: it is zeroized
                    // when the command is dropped after handling.
                    let password = SecretString::from(std::mem::take(&mut self.password));
                    self.send(Command::Login {
                        username: self.username.trim().to_string(),
                        password,
                    });
                }

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

    fn twofa_ui(&mut self, ui: &mut egui::Ui, methods: &[TwoFactorMethod]) {
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
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let grouped = search(&sections(state), &self.search);
                    friend_section(ui, "Favorites", &grouped.favorites);
                    friend_section(ui, "In Game", &grouped.in_game);
                    friend_section(ui, "Web / App", &grouped.web_active);
                    friend_section(ui, "Offline", &grouped.offline);
                });
            });
    }

    fn dashboard_panel(&self, ui: &mut egui::Ui, state: &AppState) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Friends' worlds");
            ui.separator();
            let entries = dashboard(state);
            if entries.is_empty() {
                ui.label("No friends in a world right now.");
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("worlds_grid")
                    .num_columns(2)
                    .spacing(egui::vec2(12.0, 12.0))
                    .show(ui, |ui| {
                        for entry in &entries {
                            world_card(ui, entry);
                            ui.end_row();
                        }
                    });
            });
        });
    }

    fn main_ui(&mut self, ui: &mut egui::Ui, state: &AppState) {
        self.top_bar(ui, state);
        self.friends_panel(ui, state);
        self.dashboard_panel(ui, state);
    }
}

impl eframe::App for VrcNextApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let state = self.state.snapshot();
        match &state.auth {
            AuthState::LoggedOut => self.login_ui(ui),
            AuthState::Needs2fa(methods) => self.twofa_ui(ui, methods),
            AuthState::Authenticated => self.main_ui(ui, &state),
        }
        // Surface backend errors that arrived without a matching event.
        if state.last_error.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}

/// One collapsible friend section inside the left panel.
fn friend_section(ui: &mut egui::Ui, title: &str, friends: &[Friend]) {
    egui::CollapsingHeader::new(format!("{title} ({})", friends.len()))
        .default_open(true)
        .show(ui, |ui| {
            if friends.is_empty() {
                ui.weak("—");
                return;
            }
            for friend in friends {
                friend_row(ui, friend);
            }
        });
    ui.add_space(4.0);
}

/// A single friend row: presence dot, name, trust badge, location/status.
fn friend_row(ui: &mut egui::Ui, friend: &Friend) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("●")
                .color(presence_color(friend))
                .size(14.0),
        );
        ui.strong(&friend.display_name);
        let (color, label) = trust_badge(friend.trust_rank);
        ui.colored_label(color, label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let detail = if friend.location.is_empty() {
                friend.status.clone()
            } else {
                friend.location.clone()
            };
            ui.weak(detail);
        });
    });
}

/// One dashboard card: world name, friend count and the friends inside.
fn world_card(ui: &mut egui::Ui, entry: &vrc_core::DashboardEntry) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_min_width(220.0);
            ui.horizontal(|ui| {
                ui.strong(vrc_core::world_name_label(entry));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!(
                        "{} friend{}",
                        entry.group.friends.len(),
                        plural(entry.group.friends.len())
                    ));
                });
            });
            ui.small(
                egui::RichText::new(&entry.group.location)
                    .monospace()
                    .weak(),
            );
            ui.separator();
            for friend in &entry.group.friends {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("●")
                            .color(presence_color(friend))
                            .size(12.0),
                    );
                    ui.label(&friend.display_name);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (_, label) = trust_badge(friend.trust_rank);
                        ui.weak(label);
                    });
                });
            }
        });
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
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

    #[test]
    fn plural_only_for_non_one() {
        assert_eq!(plural(0), "s");
        assert_eq!(plural(1), "");
        assert_eq!(plural(2), "s");
    }
}
