//! M7 tools tabs: Launcher, Chatbox, OSC parameters and the events
//! calendar.
//!
//! Same contract as the rest of the UI (RULES.md §7): read [`AppState`]
//! snapshots, push [`Command`]s — no I/O, no business logic. All helpers
//! are pure and unit-tested below.

use eframe::egui;
use vrc_core::{
    parse_param_value, valid_param_addr, CalendarEvent, Command, Companion, DiscoverScope,
    LaunchPrefs, ParamValue, CHATBOX_MAX_CHARS,
};

use crate::VrcNextApp;

/// Build [`LaunchPrefs`] from the launch tab's edit buffers.
fn collect_launch_prefs(app: &VrcNextApp) -> LaunchPrefs {
    LaunchPrefs {
        no_vr: app.launch_no_vr,
        profile: app.launch_profile.trim().parse().unwrap_or(0),
        fps: app.launch_fps.trim().parse().unwrap_or(0),
        osc_input_port: app.launch_osc_in.trim().parse().unwrap_or(0),
        osc_output_port: app.launch_osc_out.trim().parse().unwrap_or(0),
        world_id: app.launch_world.trim().to_string(),
        instance_id: app.launch_instance.trim().to_string(),
    }
}

/// Format a received/sent parameter value for display.
pub(crate) fn fmt_param_value(value: ParamValue) -> String {
    match value {
        ParamValue::Float(v) => format!("{v}"),
        ParamValue::Int(v) => v.to_string(),
        ParamValue::Bool(v) => v.to_string(),
    }
}

/// `true` while the OSC address is sendable — mirrors the core-side
/// validation so the button can grey out early.
pub(crate) fn osc_addr_ok(addr: &str) -> bool {
    valid_param_addr(addr.trim())
}

// ---------------------------------------------------------------------------
// Launcher tab
// ---------------------------------------------------------------------------

pub(crate) fn launcher_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &vrc_core::AppState) {
    // First visit: seed the edit buffers from persisted prefs so Apply is
    // a no-op until the user changes something.
    if !app.tools_buf_ready {
        let prefs = &state.tools.launch;
        app.launch_no_vr = prefs.no_vr;
        app.launch_profile = if prefs.profile > 0 {
            prefs.profile.to_string()
        } else {
            String::new()
        };
        app.launch_fps = if prefs.fps > 0 {
            prefs.fps.to_string()
        } else {
            String::new()
        };
        app.launch_osc_in = if prefs.osc_input_port > 0 {
            prefs.osc_input_port.to_string()
        } else {
            String::new()
        };
        app.launch_osc_out = if prefs.osc_output_port > 0 {
            prefs.osc_output_port.to_string()
        } else {
            String::new()
        };
        app.launch_world = prefs.world_id.clone();
        app.launch_instance = prefs.instance_id.clone();
        app.vrchat_path_input = state.tools.vrchat_path.clone().unwrap_or_default();
        app.auto_start_companion = state.tools.auto_start_companion;
        app.tools_buf_ready = true;
    }
    let tools = &state.tools;
    egui::ScrollArea::vertical().show(ui, |ui| {
        // Install status + override.
        ui.horizontal(|ui| {
            ui.strong("VRChat install:");
            match (&tools.vrchat_path, &tools.vrchat_found) {
                (Some(path), _) => ui.label(format!("override: {path}")),
                (None, Some(dir)) => ui.label(dir).on_hover_text("found via Steam"),
                (None, None) => ui.colored_label(egui::Color32::LIGHT_RED, "not found"),
            };
        });
        ui.horizontal(|ui| {
            ui.label("Path override:");
            ui.add(
                egui::TextEdit::singleline(&mut app.vrchat_path_input)
                    .hint_text("C:\\…\\VRChat (empty = Steam auto-detect)")
                    .desired_width(320.0),
            );
            if ui.button("Set").clicked() {
                let path = app.vrchat_path_input.trim().to_string();
                app.send(Command::SetVrchatPath {
                    path: (!path.is_empty()).then_some(path),
                });
            }
            if tools.vrchat_path.is_some() && ui.button("Auto-detect").clicked() {
                app.send(Command::SetVrchatPath { path: None });
            }
        });
        ui.separator();

        // Launch options — plain string buffers, applied via SetLaunchPrefs.
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.launch_no_vr, "--no-vr (desktop mode)");
            ui.label("profile:");
            ui.add(
                egui::TextEdit::singleline(&mut app.launch_profile)
                    .desired_width(50.0)
                    .hint_text("0"),
            );
            ui.label("fps:");
            ui.add(
                egui::TextEdit::singleline(&mut app.launch_fps)
                    .desired_width(50.0)
                    .hint_text("0"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("OSC ports (in/out):");
            ui.add(
                egui::TextEdit::singleline(&mut app.launch_osc_in)
                    .desired_width(50.0)
                    .hint_text("9000"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut app.launch_osc_out)
                    .desired_width(50.0)
                    .hint_text("9001"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Join world:");
            ui.add(
                egui::TextEdit::singleline(&mut app.launch_world)
                    .hint_text("wrld_…")
                    .desired_width(280.0),
            );
            ui.label("instance:");
            ui.add(
                egui::TextEdit::singleline(&mut app.launch_instance)
                    .hint_text("optional")
                    .desired_width(160.0),
            );
        });
        ui.horizontal(|ui| {
            if ui.button("Apply options").clicked() {
                app.send(Command::SetLaunchPrefs {
                    prefs: collect_launch_prefs(app),
                });
            }
            if ui
                .button("Launch VRChat")
                .on_hover_text("Apply options, then spawn VRChat.exe")
                .clicked()
            {
                app.send(Command::SetLaunchPrefs {
                    prefs: collect_launch_prefs(app),
                });
                app.send(Command::LaunchVrchat);
            }
            if ui
                .checkbox(&mut app.auto_start_companion, "auto-start companions")
                .changed()
            {
                app.send(Command::SetAutoStartCompanion {
                    enabled: app.auto_start_companion,
                });
            }
        });
        if let Some(last) = &tools.last_launch {
            ui.weak(format!("last launch: {last}"));
        }
        ui.separator();

        // Companions.
        ui.horizontal(|ui| {
            ui.strong("Companions");
            ui.label("name:");
            ui.add(
                egui::TextEdit::singleline(&mut app.companion_name)
                    .desired_width(120.0)
                    .hint_text("name"),
            );
            ui.label("path:");
            ui.add(
                egui::TextEdit::singleline(&mut app.companion_path)
                    .desired_width(220.0)
                    .hint_text("C:\\…\\app.exe"),
            );
            ui.label("args:");
            ui.add(
                egui::TextEdit::singleline(&mut app.companion_args)
                    .desired_width(140.0)
                    .hint_text("--flag value"),
            );
            if ui.button("Add").clicked() {
                let args: Vec<String> = app
                    .companion_args
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                app.send(Command::AddCompanion {
                    companion: Companion {
                        name: app.companion_name.trim().to_string(),
                        path: app.companion_path.trim().to_string(),
                        args,
                    },
                });
                app.companion_name.clear();
                app.companion_path.clear();
                app.companion_args.clear();
            }
        });
        if tools.companions.is_empty() {
            ui.weak("No companions configured.");
        }
        for (index, companion) in tools.companions.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(&companion.name);
                ui.weak(&companion.path);
                if !companion.args.is_empty() {
                    ui.weak(companion.args.join(" "));
                }
                if ui.small_button("Launch").clicked() {
                    app.send(Command::LaunchCompanion { index });
                }
                if ui.small_button("Remove").clicked() {
                    app.send(Command::RemoveCompanion { index });
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Chatbox tab
// ---------------------------------------------------------------------------

pub(crate) fn chatbox_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, _state: &vrc_core::AppState) {
    let out = ui.add(
        egui::TextEdit::multiline(&mut app.chatbox_text)
            .hint_text("Chatbox message (144 chars max)…")
            .desired_width(f32::INFINITY)
            .desired_rows(3),
    );
    ui.horizontal(|ui| {
        ui.checkbox(&mut app.chatbox_immediate, "send immediately");
        if ui
            .button("Send")
            .on_hover_text("Throttled to one message per 1.5 s")
            .clicked()
        {
            let text = std::mem::take(&mut app.chatbox_text);
            app.send(Command::SendChatbox {
                text,
                immediate: app.chatbox_immediate,
            });
        }
        if ui.button("Typing…").clicked() {
            app.send(Command::SendChatboxTyping { typing: true });
        }
        if ui.button("Typing off").clicked() {
            app.send(Command::SendChatboxTyping { typing: false });
        }
        let _ = out;
    });
    // Char counter mirrors the core sanitizer (control chars stripped,
    // clamped to CHATBOX_MAX_CHARS on send).
    let len = app.chatbox_text.chars().filter(|c| !c.is_control()).count();
    ui.colored_label(
        if len > CHATBOX_MAX_CHARS {
            egui::Color32::LIGHT_RED
        } else {
            egui::Color32::GRAY
        },
        format!("{len}/{CHATBOX_MAX_CHARS}"),
    );
    ui.separator();
    ui.weak("Messages are sanitized (control chars stripped) and clamped to 144 characters.");
}

// ---------------------------------------------------------------------------
// OSC parameters tab
// ---------------------------------------------------------------------------

pub(crate) fn osc_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &vrc_core::AppState) {
    let osc = &state.tools.osc;
    ui.horizontal(|ui| {
        if osc.listening {
            ui.colored_label(
                egui::Color32::from_rgb(0x3f, 0xa7, 0x5d),
                format!("● listening on 127.0.0.1:{}", osc.listen_port),
            );
            if ui.button("Stop").clicked() {
                app.send(Command::StopOscListen);
            }
        } else {
            ui.weak("listener stopped");
            if ui.button("Listen on 9001").clicked() {
                app.send(Command::StartOscListen { port: 9001 });
            }
        }
    });
    ui.separator();

    // Send one parameter.
    ui.horizontal(|ui| {
        ui.label("address:");
        ui.add(
            egui::TextEdit::singleline(&mut app.osc_addr)
                .hint_text("/avatar/parameters/Speed")
                .desired_width(240.0),
        );
        ui.label("value:");
        ui.add(
            egui::TextEdit::singleline(&mut app.osc_value)
                .hint_text("0.5 / 1 / true")
                .desired_width(80.0),
        );
        let addr_ok = osc_addr_ok(&app.osc_addr);
        let value = parse_param_value(&app.osc_value);
        if ui
            .add_enabled(addr_ok && value.is_some(), egui::Button::new("Send"))
            .clicked()
        {
            if let Some(value) = value {
                app.send(Command::SendOscParam {
                    address: app.osc_addr.trim().to_string(),
                    value,
                });
            }
        }
        if !addr_ok && !app.osc_addr.is_empty() {
            ui.weak("address must start with /avatar/parameters/");
        }
    });
    ui.separator();

    // Latest received/sent parameters.
    ui.strong(format!("Parameters ({})", osc.params.len()));
    egui::ScrollArea::vertical().show(ui, |ui| {
        let mut entries: Vec<_> = osc.params.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        egui::Grid::new("osc_params")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for (addr, value) in entries {
                    ui.monospace(addr);
                    ui.label(fmt_param_value(*value));
                    ui.end_row();
                }
            });
    });
}

// ---------------------------------------------------------------------------
// Calendar tab
// ---------------------------------------------------------------------------

/// Render one event row: title, time, category, follow toggle.
fn event_row(app: &mut VrcNextApp, ui: &mut egui::Ui, event: &CalendarEvent, group_id: &str) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.strong(&event.title);
            ui.small(format!(
                "{} · {} · {} interested",
                event.starts_at, event.category, event.interested_user_count
            ));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Follow/unfollow only exists for group calendars — the
            // endpoint is /calendar/{groupId}/{id}/follow and rejects
            // usr_… owner ids, so hide the toggle for user-owned events.
            if !group_id.starts_with("grp_") {
                return;
            }
            let following = event.user_interest.as_ref().is_some_and(|i| i.is_following);
            if ui
                .selectable_label(following, if following { "following" } else { "follow" })
                .clicked()
            {
                app.send(Command::FollowCalendarEvent {
                    group_id: group_id.to_string(),
                    calendar_id: event.id.clone(),
                    is_following: !following,
                });
            }
        });
    });
}

pub(crate) fn calendar_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &vrc_core::AppState) {
    let cal = &state.calendar;
    egui::ScrollArea::vertical().show(ui, |ui| {
        // My calendar (month) + featured-style discovery + search.
        ui.horizontal(|ui| {
            ui.label("Month:");
            ui.add(
                egui::TextEdit::singleline(&mut app.calendar_month)
                    .hint_text("YYYY-MM-DD (API month key)")
                    .desired_width(160.0),
            );
            if ui.button("Load").clicked() {
                let date = app.calendar_month.trim().to_string();
                app.send(Command::LoadCalendarMonth {
                    date: (!date.is_empty()).then_some(date),
                    offset: 0,
                });
            }
            ui.separator();
            egui::ComboBox::from_label("Discover")
                .selected_text(match app.calendar_scope {
                    DiscoverScope::All => "all",
                    DiscoverScope::Live => "live",
                    DiscoverScope::Upcoming => "upcoming",
                })
                .show_ui(ui, |ui| {
                    for scope in [
                        DiscoverScope::Upcoming,
                        DiscoverScope::Live,
                        DiscoverScope::All,
                    ] {
                        let label = match scope {
                            DiscoverScope::All => "all",
                            DiscoverScope::Live => "live",
                            DiscoverScope::Upcoming => "upcoming",
                        };
                        ui.selectable_value(&mut app.calendar_scope, scope, label);
                    }
                });
            if ui.button("Discover").clicked() {
                app.send(Command::LoadCalendarDiscover {
                    scope: app.calendar_scope,
                    cursor: None,
                });
            }
            if let Some(d) = &cal.discover {
                if d.next_cursor.is_some() && ui.button("More").clicked() {
                    app.send(Command::LoadCalendarDiscover {
                        scope: app.calendar_scope,
                        cursor: d.next_cursor.clone(),
                    });
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add(
                egui::TextEdit::singleline(&mut app.calendar_search)
                    .hint_text("event title…")
                    .desired_width(220.0),
            );
            if ui.button("Search").clicked() {
                app.send(Command::LoadCalendarSearch {
                    query: app.calendar_search.trim().to_string(),
                    offset: 0,
                });
            }
        });
        ui.horizontal(|ui| {
            ui.label("Group calendar:");
            ui.add(
                egui::TextEdit::singleline(&mut app.calendar_group)
                    .hint_text("grp_…")
                    .desired_width(240.0),
            );
            if ui.button("Load").clicked() {
                app.send(Command::LoadGroupCalendar {
                    group_id: app.calendar_group.trim().to_string(),
                    date: None,
                    offset: 0,
                });
            }
            if ui.button("Next event").clicked() {
                app.send(Command::LoadGroupNextEvent {
                    group_id: app.calendar_group.trim().to_string(),
                });
            }
        });
        ui.separator();

        // Group "next" banner.
        if let Some(event) = cal.next_events.get(app.calendar_group.trim()) {
            match event {
                Some(ev) => {
                    ui.label(format!("Next up: {} — {}", ev.title, ev.starts_at));
                }
                None => {
                    ui.weak("No upcoming events for that group.");
                }
            }
        }

        // Event lists: group calendar first (it has a group id for follow),
        // then month, search and discovery.
        if let Some((group_id, page)) = &cal.group {
            ui.collapsing(format!("Group calendar ({})", page.results.len()), |ui| {
                for event in &page.results {
                    event_row(app, ui, event, group_id);
                }
            });
        }
        if let Some((_, page)) = &cal.month {
            ui.collapsing(format!("My calendar ({})", page.results.len()), |ui| {
                for event in &page.results {
                    event_row(app, ui, event, &event.owner_id);
                }
            });
        }
        if let Some((query, page)) = &cal.search {
            ui.collapsing(
                format!("Search “{query}” ({})", page.results.len()),
                |ui| {
                    for event in &page.results {
                        event_row(app, ui, event, &event.owner_id);
                    }
                },
            );
        }
        if let Some(d) = &cal.discover {
            ui.collapsing(format!("Discover ({})", d.events.len()), |ui| {
                for event in &d.events {
                    event_row(app, ui, event, &event.owner_id);
                }
            });
        }
        if cal.month.is_none()
            && cal.search.is_none()
            && cal.group.is_none()
            && cal.discover.is_none()
        {
            ui.label(
                "No calendar data yet — load a month, discovery feed, search or group calendar.",
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_param_value_covers_all_kinds() {
        assert_eq!(parse_param_value("true"), Some(ParamValue::Bool(true)));
        assert_eq!(parse_param_value(" FALSE "), Some(ParamValue::Bool(false)));
        assert_eq!(parse_param_value("42"), Some(ParamValue::Int(42)));
        assert_eq!(parse_param_value("-7"), Some(ParamValue::Int(-7)));
        assert_eq!(parse_param_value("0.5"), Some(ParamValue::Float(0.5)));
        assert_eq!(parse_param_value(""), None);
        assert_eq!(parse_param_value("abc"), None);
    }

    #[test]
    fn fmt_param_value_renders() {
        assert_eq!(fmt_param_value(ParamValue::Bool(true)), "true");
        assert_eq!(fmt_param_value(ParamValue::Int(-3)), "-3");
        assert_eq!(fmt_param_value(ParamValue::Float(1.5)), "1.5");
    }

    #[test]
    fn launch_prefs_buffer_defaults() {
        let prefs = LaunchPrefs::default();
        assert!(!prefs.no_vr);
        assert_eq!(prefs.profile, 0);
    }
}
