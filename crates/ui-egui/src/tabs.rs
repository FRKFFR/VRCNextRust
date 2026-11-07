//! Local-data tabs (M3): Timeline, Friend history and Stats.
//!
//! Pure presentation like the rest of the UI (RULES.md §7): tabs read
//! [`AppState`] snapshots and push [`Command`]s — no store or filesystem
//! access. All formatting helpers are pure and unit-tested below.

use eframe::egui;
use time::{Duration, OffsetDateTime};
use vrc_core::{
    AppState, Command, FriendHistoryFilter, StatsRange, TimelineFilter, TimelineKind,
    LOCAL_PAGE_SIZE,
};

use crate::VrcNextApp;

/// Command dispatch for the data tabs: (re)load the current page of the
/// active view.
impl VrcNextApp {
    pub(crate) fn reload_timeline(&self) {
        self.send(Command::LoadTimeline {
            page: self.timeline_page,
            filter: timeline_filter(self),
        });
    }

    pub(crate) fn reload_history(&self) {
        self.send(Command::LoadFriendHistory {
            page: self.history_page,
            filter: history_filter(self),
        });
    }

    pub(crate) fn reload_stats(&self) {
        self.send(Command::LoadStats {
            range: self.stats_preset.range(OffsetDateTime::now_utc()),
        });
    }
}

/// Which central-panel tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    /// Friends' worlds dashboard (the M2 view).
    Dashboard,
    /// Notifications + NotificationV2 list.
    Notifications,
    /// The loaded user's profile, note and social actions.
    Profile,
    /// Personal timeline parsed from VRChat logs.
    Timeline,
    /// Friend presence/location history from pipeline events.
    FriendHistory,
    /// Time-spent statistics.
    Stats,
    /// World search/browse (M5).
    Worlds,
    /// Group search/browse (M5).
    Groups,
    /// Avatar search/switch (M5).
    Avatars,
    /// World/avatar favorites (M5).
    Favorites,
}

/// Stats time-range presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatsPreset {
    /// Everything ever recorded.
    AllTime,
    /// The last 24 hours.
    Last24h,
    /// The last 7 days.
    Last7d,
    /// The last 30 days.
    Last30d,
}

impl StatsPreset {
    /// The [`StatsRange`] this preset stands for, relative to `now`.
    fn range(self, now: OffsetDateTime) -> StatsRange {
        let since = match self {
            Self::AllTime => None,
            Self::Last24h => Some(now - Duration::hours(24)),
            Self::Last7d => Some(now - Duration::days(7)),
            Self::Last30d => Some(now - Duration::days(30)),
        };
        StatsRange { since, until: None }
    }

    fn label(self) -> &'static str {
        match self {
            Self::AllTime => "All time",
            Self::Last24h => "Last 24 hours",
            Self::Last7d => "Last 7 days",
            Self::Last30d => "Last 30 days",
        }
    }
}

/// Favorite records power the favorite toggles on the M5 tabs; load them
/// once per session (they are cheap and rarely change).
fn ensure_favorite_records(app: &VrcNextApp) {
    if app.state.snapshot().favorite_records.is_empty() {
        app.send(Command::RefreshFavorites);
    }
}

/// Switch tabs; entering a data tab (re)loads its current page.
pub(crate) fn select_tab(app: &mut VrcNextApp, tab: Tab) {
    if app.tab == tab {
        return;
    }
    app.tab = tab;
    match tab {
        Tab::Dashboard => {}
        Tab::Notifications => app.send(Command::RefreshNotifications),
        Tab::Profile => {}
        Tab::Timeline => app.reload_timeline(),
        Tab::FriendHistory => app.reload_history(),
        Tab::Stats => app.reload_stats(),
        Tab::Worlds => {
            app.send(Command::SearchWorlds {
                query: app.world_query.clone(),
                offset: 0,
            });
            ensure_favorite_records(app);
        }
        Tab::Groups => {}
        Tab::Avatars => {
            app.send(Command::SearchAvatars {
                query: app.avatar_query.clone(),
                offset: 0,
                own: app.avatar_own,
            });
            ensure_favorite_records(app);
        }
        Tab::Favorites => app.send(Command::RefreshFavorites),
    }
}

/// Tab bar + the active tab's content (rendered inside the central panel).
pub(crate) fn central_panel(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.horizontal(|ui| {
            for (tab, label) in [
                (Tab::Dashboard, "Dashboard"),
                (Tab::Notifications, "Notifications"),
                (Tab::Profile, "Profile"),
                (Tab::Timeline, "Timeline"),
                (Tab::FriendHistory, "Friend history"),
                (Tab::Stats, "Stats"),
                (Tab::Worlds, "Worlds"),
                (Tab::Groups, "Groups"),
                (Tab::Avatars, "Avatars"),
                (Tab::Favorites, "Favorites"),
            ] {
                if ui.selectable_label(app.tab == tab, label).clicked() {
                    select_tab(app, tab);
                }
            }
        });
        ui.separator();
        match app.tab {
            Tab::Dashboard => dashboard_tab(app, ui, state),
            Tab::Notifications => crate::social::notifications_tab(app, ui, state),
            Tab::Profile => crate::social::profile_tab(app, ui, state),
            Tab::Timeline => timeline_tab(app, ui, state),
            Tab::FriendHistory => history_tab(app, ui, state),
            Tab::Stats => stats_tab(app, ui, state),
            Tab::Worlds => crate::browse::worlds_tab(app, ui, state),
            Tab::Groups => crate::browse::groups_tab(app, ui, state),
            Tab::Avatars => crate::browse::avatars_tab(app, ui, state),
            Tab::Favorites => crate::browse::favorites_tab(app, ui, state),
        }
    });
}

// ---------------------------------------------------------------------------
// Dashboard (moved verbatim from the old central panel)
// ---------------------------------------------------------------------------

fn dashboard_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    ui.heading("Friends' worlds");
    ui.separator();

    // The loaded instance's details + which friends are inside it.
    if let Some(instance) = &state.instance {
        instance_card(app, ui, state, instance);
        ui.separator();
    }

    let entries = vrc_core::dashboard(state);
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
                    world_card(app, ui, entry);
                    ui.end_row();
                }
            });
    });
}

/// Details of the instance loaded via `LoadInstance`, including the
/// friends currently located there (`friends_in_instance`).
fn instance_card(
    app: &mut VrcNextApp,
    ui: &mut egui::Ui,
    state: &AppState,
    instance: &vrc_core::Instance,
) {
    let inside =
        vrc_core::friends_in_instance(&state.friends, &instance.world_id, &instance.instance_id);
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(if instance.world.name.is_empty() {
                    instance.world_id.clone()
                } else {
                    instance.world.name.clone()
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!(
                        "{} user{}",
                        instance.user_count.max(instance.n_users),
                        if instance.user_count.max(instance.n_users) == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ));
                });
            });
            ui.small(
                egui::RichText::new(format!(
                    "{} · {} · {}",
                    instance.kind, instance.instance_id, instance.short_name
                ))
                .monospace()
                .weak(),
            );
            ui.separator();
            if inside.is_empty() {
                ui.weak("No friends in this instance.");
            } else {
                for friend in inside {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("●")
                                .color(crate::presence_color(friend))
                                .size(12.0),
                        );
                        ui.label(&friend.display_name);
                        if ui.small_button("Profile").clicked() {
                            app.send(Command::LoadProfile {
                                user_id: friend.id.clone(),
                            });
                            app.tab = Tab::Profile;
                        }
                        if ui
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
                    });
                }
            }
            ui.separator();
            if ui
                .small_button("Invite myself")
                .on_hover_text("Send yourself an invite to this instance")
                .clicked()
            {
                app.send(Command::InviteMyselfToInstance {
                    world_id: instance.world_id.clone(),
                    instance_id: instance.instance_id.clone(),
                });
            }
        });
}

fn world_card(app: &mut VrcNextApp, ui: &mut egui::Ui, entry: &vrc_core::DashboardEntry) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_min_width(220.0);
            ui.horizontal(|ui| {
                // Clicking the card loads the instance's details
                // (`GET /instances/{world}:{instance}`) into state.
                if ui
                    .strong(vrc_core::world_title(entry))
                    .on_hover_text("Load instance details")
                    .clicked()
                {
                    app.send(Command::LoadInstance {
                        world_id: entry.world_id.clone(),
                        instance_id: entry.instance_id.clone(),
                    });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!(
                        "{} friend{}",
                        entry.friends.len(),
                        plural(entry.friends.len())
                    ));
                });
            });
            ui.small(egui::RichText::new(&entry.instance_id).monospace().weak());
            ui.separator();
            for friend in &entry.friends {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("●")
                            .color(crate::presence_color(friend))
                            .size(12.0),
                    );
                    ui.label(&friend.display_name);
                    if ui
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
                        let (_, label) = crate::trust_badge(friend.trust_rank);
                        ui.weak(label);
                    });
                });
            }
        });
}

// ---------------------------------------------------------------------------
// Timeline tab
// ---------------------------------------------------------------------------

fn timeline_filter(app: &VrcNextApp) -> TimelineFilter {
    TimelineFilter {
        kind: app.timeline_kind,
        query: non_empty(&app.timeline_query),
        ..TimelineFilter::default()
    }
}

fn timeline_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    ui.horizontal(|ui| {
        let query = egui::TextEdit::singleline(&mut app.timeline_query)
            .hint_text("Filter…")
            .desired_width(220.0)
            .show(ui);
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (enter && query.response.has_focus()) || ui.button("Apply").clicked() {
            app.timeline_page = 0;
            app.reload_timeline();
        }
        ui.separator();
        egui::ComboBox::from_label("Kind")
            .selected_text(kind_label(app.timeline_kind))
            .show_ui(ui, |ui| {
                for kind in [
                    None,
                    Some(TimelineKind::JoinedWorld),
                    Some(TimelineKind::LeftWorld),
                    Some(TimelineKind::PlayerJoined),
                    Some(TimelineKind::PlayerLeft),
                    Some(TimelineKind::PhotoTaken),
                ] {
                    if ui
                        .selectable_value(&mut app.timeline_kind, kind, kind_label(kind))
                        .changed()
                    {
                        app.timeline_page = 0;
                        app.reload_timeline();
                    }
                }
            });
        if ui.button("Refresh").clicked() {
            app.reload_timeline();
        }
    });
    ui.separator();

    let Some(loaded) = &state.timeline else {
        ui.label("No timeline data yet — is VRChat running?");
        return;
    };
    if let Some(delta) = pager(ui, app.timeline_page, loaded.total) {
        app.timeline_page = (i64::from(app.timeline_page) + delta).max(0) as u32;
        app.reload_timeline();
    }
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("timeline_grid")
            .num_columns(3)
            .striped(true)
            .spacing(egui::vec2(12.0, 4.0))
            .show(ui, |ui| {
                ui.strong("Time");
                ui.strong("Event");
                ui.strong("Details");
                ui.end_row();
                for entry in &loaded.entries {
                    ui.label(fmt_ts(entry.ts));
                    ui.label(kind_label(Some(entry.kind)));
                    ui.label(timeline_summary(entry));
                    ui.end_row();
                }
            });
    });
}

// ---------------------------------------------------------------------------
// Friend history tab
// ---------------------------------------------------------------------------

fn history_filter(app: &VrcNextApp) -> FriendHistoryFilter {
    FriendHistoryFilter {
        user_id: app.history_user.clone(),
        query: non_empty(&app.history_query),
        ..FriendHistoryFilter::default()
    }
}

fn history_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    ui.horizontal(|ui| {
        // Friend picker: all friends + "All friends".
        let mut friends: Vec<_> = state.friends.values().collect();
        friends.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        // A `history_user` whose friend was removed still filters the
        // query — show the raw id rather than the misleading "All friends".
        let selected = app
            .history_user
            .as_ref()
            .map(|id| {
                friends
                    .iter()
                    .find(|f| &f.id == id)
                    .map(|f| f.display_name.clone())
                    .unwrap_or_else(|| id.clone())
            })
            .unwrap_or_else(|| "All friends".to_string());
        egui::ComboBox::from_label("Friend")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(app.history_user.is_none(), "All friends")
                    .clicked()
                {
                    app.history_user = None;
                    app.history_page = 0;
                    app.reload_history();
                }
                for friend in friends {
                    if ui
                        .selectable_label(
                            app.history_user.as_deref() == Some(friend.id.as_str()),
                            &friend.display_name,
                        )
                        .clicked()
                    {
                        app.history_user = Some(friend.id.clone());
                        app.history_page = 0;
                        app.reload_history();
                    }
                }
            });
        ui.separator();
        let query = egui::TextEdit::singleline(&mut app.history_query)
            .hint_text("Filter…")
            .desired_width(180.0)
            .show(ui);
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (enter && query.response.has_focus()) || ui.button("Apply").clicked() {
            app.history_page = 0;
            app.reload_history();
        }
        if ui.button("Refresh").clicked() {
            app.reload_history();
        }
    });
    ui.separator();

    let Some(loaded) = &state.friend_history else {
        ui.label("No friend history yet — pipeline events are recorded while connected.");
        return;
    };
    if let Some(delta) = pager(ui, app.history_page, loaded.total) {
        app.history_page = (i64::from(app.history_page) + delta).max(0) as u32;
        app.reload_history();
    }
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("history_grid")
            .num_columns(4)
            .striped(true)
            .spacing(egui::vec2(12.0, 4.0))
            .show(ui, |ui| {
                ui.strong("Time");
                ui.strong("Friend");
                ui.strong("Event");
                ui.strong("Location");
                ui.end_row();
                for entry in &loaded.entries {
                    ui.label(fmt_ts(entry.ts));
                    ui.label(&entry.display_name);
                    ui.label(event_label(entry.event));
                    ui.label(history_location(entry));
                    ui.end_row();
                }
            });
    });
}

// ---------------------------------------------------------------------------
// Stats tab
// ---------------------------------------------------------------------------

fn stats_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Range")
            .selected_text(app.stats_preset.label())
            .show_ui(ui, |ui| {
                for preset in [
                    StatsPreset::AllTime,
                    StatsPreset::Last24h,
                    StatsPreset::Last7d,
                    StatsPreset::Last30d,
                ] {
                    if ui
                        .selectable_value(&mut app.stats_preset, preset, preset.label())
                        .changed()
                    {
                        app.reload_stats();
                    }
                }
            });
        if ui.button("Refresh").clicked() {
            app.reload_stats();
        }
    });
    ui.separator();

    let Some(stats) = &state.stats else {
        ui.label("No statistics yet.");
        return;
    };
    ui.strong(format!(
        "{} across {} session{}",
        format_duration(stats.total_seconds),
        stats.session_count,
        plural(stats.session_count as usize),
    ));
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.strong("Top worlds");
            ui.separator();
            if stats.top_worlds.is_empty() {
                ui.weak("—");
            }
            for world in &stats.top_worlds {
                ui.label(format!(
                    "{} — {} ({} session{})",
                    world_label(world),
                    format_duration(world.seconds),
                    world.sessions,
                    plural(world.sessions as usize),
                ));
            }
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.strong("Time with friends");
            ui.separator();
            if stats.top_friends.is_empty() {
                ui.weak("—");
            }
            for friend in &stats.top_friends {
                ui.label(format!(
                    "{} — {}",
                    friend.display_name,
                    format_duration(friend.seconds),
                ));
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Shared bits
// ---------------------------------------------------------------------------

/// Prev/Next pager; returns the page delta to apply (-1/+1) or `None`.
fn pager(ui: &mut egui::Ui, page: u32, total: u64) -> Option<i64> {
    let pages = total.div_ceil(u64::from(LOCAL_PAGE_SIZE)).max(1);
    let mut delta = None;
    ui.horizontal(|ui| {
        ui.weak(format!("page {} / {pages}", page + 1));
        if ui
            .add_enabled(page > 0, egui::Button::new("◀ Prev"))
            .clicked()
        {
            delta = Some(-1);
        }
        let next_ok = u64::from(page + 1) * u64::from(LOCAL_PAGE_SIZE) < total;
        if ui
            .add_enabled(next_ok, egui::Button::new("Next ▶"))
            .clicked()
        {
            delta = Some(1);
        }
    });
    delta
}

fn non_empty(query: &str) -> Option<String> {
    let trimmed = query.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

// Formatting helpers are shared with the CLI via `vrc_core::format`;
// re-exported here so the tab code reads naturally.
pub(crate) use vrc_core::format::{
    event_label, fmt_ts, format_duration, history_location, kind_label, timeline_summary,
    world_label,
};

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time};

    fn ts() -> OffsetDateTime {
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::November, 5).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        )
        .assume_utc()
    }

    // UI-only assertions live here — the format helpers themselves are
    // covered by crates/core/src/format.rs tests.

    #[test]
    fn stats_preset_ranges() {
        let now = ts();
        assert!(StatsPreset::AllTime.range(now).since.is_none());
        let week = StatsPreset::Last7d.range(now);
        assert_eq!(week.since.unwrap(), now - Duration::days(7));
        assert!(week.until.is_none());
    }
}
