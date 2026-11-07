//! Social tabs (M4): Notifications, Profile, multi-invite picker and the
//! destructive-action confirm dialog.
//!
//! Pure presentation like the rest of the UI (RULES.md §7): everything
//! reads [`AppState`] snapshots and pushes [`Command`]s. Remote text
//! (names, messages, notes) renders as plain labels — never markup.

use eframe::egui;
use vrc_core::{
    friend_status_label, moderation_label, notification_kind_label, AppState, Command, Friend,
    PlayerModerationKind, ProfileView,
};

use crate::{ConfirmAction, VrcNextApp};

// ---------------------------------------------------------------------------
// Notifications tab
// ---------------------------------------------------------------------------

pub(crate) fn notifications_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    ui.horizontal(|ui| {
        if ui.button("Refresh").clicked() {
            app.send(Command::RefreshNotifications);
        }
        if ui
            .add_enabled(
                state.notifications.as_ref().is_some_and(|n| !n.is_empty()),
                egui::Button::new("Clear all"),
            )
            .on_hover_text("Deletes every notification")
            .clicked()
        {
            app.confirm = Some(ConfirmAction::ClearNotifications);
        }
        ui.separator();
        ui.weak(format!(
            "{} notification{}",
            state.notifications_v2.len() + state.notifications.as_ref().map_or(0, Vec::len),
            if state.notifications_v2.len() + state.notifications.as_ref().map_or(0, Vec::len) == 1
            {
                ""
            } else {
                "s"
            }
        ));
    });
    ui.separator();

    // v2 pushes can arrive before the first REST fetch; render whatever
    // exists instead of hiding them behind the `None` early-return.
    let notifications: &[vrc_core::Notification] = state.notifications.as_deref().unwrap_or(&[]);
    if notifications.is_empty() && state.notifications_v2.is_empty() {
        ui.label(if state.notifications.is_none() {
            "No notifications loaded yet."
        } else {
            "No notifications."
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for notification in notifications {
            notification_row(app, ui, state, notification);
        }
        for v2 in &state.notifications_v2 {
            notification_v2_row(ui, v2);
        }
    });
}

/// Resolve a sender id to a display name via friends/profile/current user.
fn sender_name(state: &AppState, user_id: &str, fallback: &str) -> String {
    if user_id.is_empty() {
        return if fallback.is_empty() {
            "System".to_string()
        } else {
            fallback.to_string()
        };
    }
    if let Some(friend) = state.friends.get(user_id) {
        return friend.display_name.clone();
    }
    if let Some(profile) = &state.profile {
        if profile.user.id == user_id {
            return profile.user.display_name.clone();
        }
    }
    if state.current_user.as_ref().is_some_and(|u| u.id == user_id) {
        return state
            .current_user
            .as_ref()
            .map(|u| u.display_name.clone())
            .unwrap_or_default();
    }
    if !fallback.is_empty() {
        fallback.to_string()
    } else {
        user_id.to_string()
    }
}

fn notification_row(
    app: &mut VrcNextApp,
    ui: &mut egui::Ui,
    state: &AppState,
    notification: &vrc_core::Notification,
) {
    let unread = !notification.seen;
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if unread {
                    ui.label(
                        egui::RichText::new("●")
                            .color(egui::Color32::from_rgb(0x4a, 0x7a, 0xc8))
                            .size(12.0),
                    );
                } else {
                    ui.label(" ");
                }
                ui.strong(notification_kind_label(&notification.kind));
                ui.weak(crate::fmt_short_ts(&notification.created_at));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !notification.seen && ui.small_button("Mark seen").clicked() {
                        app.send(Command::SeeNotification {
                            id: notification.id.clone(),
                        });
                    }
                    if ui.small_button("Hide").clicked() {
                        app.send(Command::HideNotification {
                            id: notification.id.clone(),
                        });
                    }
                    if notification.is_friend_request() && ui.small_button("Accept").clicked() {
                        app.send(Command::AcceptNotification {
                            id: notification.id.clone(),
                        });
                    }
                    if notification.kind == "invite" && ui.small_button("Open profile").clicked() {
                        app.send(Command::LoadProfile {
                            user_id: notification.sender_user_id.clone(),
                        });
                        app.tab = crate::tabs::Tab::Profile;
                    }
                });
            });
            ui.horizontal(|ui| {
                let name = sender_name(
                    state,
                    &notification.sender_user_id,
                    &notification.sender_username,
                );
                if ui.link(&name).on_hover_text("Open profile").clicked() {
                    app.send(Command::LoadProfile {
                        user_id: notification.sender_user_id.clone(),
                    });
                    app.tab = crate::tabs::Tab::Profile;
                }
                if !notification.message.is_empty() {
                    ui.label(format!("— {}", notification.message));
                }
            });
        });
    ui.add_space(4.0);
}

fn notification_v2_row(ui: &mut egui::Ui, notification: &vrc_core::NotificationV2) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if !notification.seen {
                    ui.label(
                        egui::RichText::new("●")
                            .color(egui::Color32::from_rgb(0x4a, 0x7a, 0xc8))
                            .size(12.0),
                    );
                } else {
                    ui.label(" ");
                }
                ui.strong(notification_kind_label(&notification.kind));
                ui.weak(crate::fmt_short_ts(&notification.created_at));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(if notification.seen { "seen" } else { "new" });
                });
            });
            if !notification.title.is_empty() {
                ui.label(&notification.title);
            }
            if !notification.message.is_empty() {
                ui.label(&notification.message);
            }
            if !notification.link.is_empty() {
                ui.weak(&notification.link);
            }
        });
    ui.add_space(4.0);
}

// ---------------------------------------------------------------------------
// Profile tab
// ---------------------------------------------------------------------------

pub(crate) fn profile_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    let Some(profile) = &state.profile else {
        ui.label("No profile loaded — click a friend's name or a notification sender.");
        return;
    };
    // Keep the note editor in sync when a different profile loads.
    if app.note_edit_user.as_deref() != Some(profile.user.id.as_str()) {
        app.note_edit = profile.user.note.clone();
        app.note_edit_user = Some(profile.user.id.clone());
    }
    let profile = profile.clone();

    ui.horizontal(|ui| {
        ui.heading(&profile.user.display_name);
        if !profile.user.username.is_empty() {
            ui.label(format!("<{}>", profile.user.username));
        }
        let (color, label) = crate::trust_badge(profile.user.trust_rank());
        ui.colored_label(color, label);
        ui.separator();
        ui.strong(friend_status_label(&profile.friend_status));
    });
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        profile_details(ui, &profile);
        ui.add_space(8.0);
        ui.strong("Note");
        ui.add_space(2.0);
        note_editor(app, ui, &profile.user.id);
        ui.add_space(8.0);
        profile_actions(app, ui, state, &profile);
    });
}

fn profile_details(ui: &mut egui::Ui, profile: &ProfileView) {
    let user = &profile.user;
    egui::Grid::new("profile_grid")
        .num_columns(2)
        .spacing(egui::vec2(8.0, 4.0))
        .show(ui, |ui| {
            if !user.bio.is_empty() {
                ui.weak("Bio");
                ui.label(&user.bio);
                ui.end_row();
            }
            if !user.status.is_empty() || !user.status_description.is_empty() {
                ui.weak("Status");
                ui.label(format!("{} {}", user.status, user.status_description));
                ui.end_row();
            }
            if !user.location.is_empty() {
                ui.weak("Location");
                ui.label(&user.location);
                ui.end_row();
            }
            ui.weak("User id");
            ui.label(egui::RichText::new(&user.id).monospace().weak());
            ui.end_row();
        });
}

fn profile_actions(
    app: &mut VrcNextApp,
    ui: &mut egui::Ui,
    state: &AppState,
    profile: &ProfileView,
) {
    let user_id = profile.user.id.clone();
    let status = profile.friend_status;

    ui.horizontal_wrapped(|ui| {
        if status.is_friend {
            if ui.button("Unfriend").clicked() {
                app.confirm = Some(ConfirmAction::Unfriend {
                    user_id: user_id.clone(),
                });
            }
        } else if status.outgoing_request {
            if ui.button("Cancel friend request").clicked() {
                app.send(Command::CancelFriendRequest {
                    user_id: user_id.clone(),
                });
            }
        } else if status.incoming_request {
            ui.weak("Friend request received — accept it from Notifications.");
        } else if ui.button("Send friend request").clicked() {
            app.send(Command::SendFriendRequest {
                user_id: user_id.clone(),
            });
        }
        ui.separator();
        if ui.button("Invite").clicked() {
            app.send(Command::InviteUser {
                user_id: user_id.clone(),
                instance_id: None,
                message_slot: None,
            });
        }
        if ui.button("Request invite").clicked() {
            app.send(Command::RequestInvite {
                user_id: user_id.clone(),
                request_slot: None,
            });
        }
    });

    ui.horizontal_wrapped(|ui| {
        let blocked = profile.has_moderation(PlayerModerationKind::Block);
        let muted = profile.has_moderation(PlayerModerationKind::Mute);
        if blocked {
            if ui.button("Unblock").clicked() {
                app.send(Command::UnmoderateUser {
                    user_id: user_id.clone(),
                    kind: PlayerModerationKind::Block,
                });
            }
        } else if ui.button("Block").clicked() {
            app.confirm = Some(ConfirmAction::Block {
                user_id: user_id.clone(),
            });
        }
        if muted {
            if ui.button("Unmute").clicked() {
                app.send(Command::UnmoderateUser {
                    user_id: user_id.clone(),
                    kind: PlayerModerationKind::Mute,
                });
            }
        } else if ui.button("Mute").clicked() {
            app.send(Command::ModerateUser {
                user_id: user_id.clone(),
                kind: PlayerModerationKind::Mute,
                confirm: false,
            });
        }
    });

    if !profile.moderations.is_empty() {
        ui.add_space(4.0);
        ui.weak("Moderations:");
        for m in &profile.moderations {
            ui.label(format!("• {}", moderation_label(&m.kind)));
        }
    }
    let _ = state;
}

// ---------------------------------------------------------------------------
// Note editor (rendered inside the profile tab's scroll area)
// ---------------------------------------------------------------------------

pub(crate) fn note_editor(app: &mut VrcNextApp, ui: &mut egui::Ui, user_id: &str) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::multiline(&mut app.note_edit)
                .hint_text("Note about this user…")
                .desired_width(f32::INFINITY)
                .desired_rows(3),
        );
        ui.vertical(|ui| {
            if ui.button("Save note").clicked() {
                app.send(Command::SetUserNote {
                    user_id: user_id.to_string(),
                    note: app.note_edit.clone(),
                });
            }
            if ui.button("Clear").clicked() {
                // Clear on the server too — an empty note removes it.
                app.send(Command::SetUserNote {
                    user_id: user_id.to_string(),
                    note: String::new(),
                });
                app.note_edit.clear();
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Multi-invite window
// ---------------------------------------------------------------------------

/// egui window listing online friends with invite checkboxes, plus live
/// progress and a cancel button while a batch runs.
pub(crate) fn multi_invite_window(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    if !app.multi_invite_open {
        return;
    }
    let mut open = true;
    egui::Window::new("Multi-invite")
        .open(&mut open)
        .default_width(320.0)
        .show(ui, |ui| {
            if let Some(progress) = &state.multi_invite {
                ui.label(format!(
                    "{}/{} done — {} sent, {} failed{}",
                    progress.done,
                    progress.total,
                    progress.succeeded,
                    progress.failed,
                    progress
                        .current
                        .as_ref()
                        .map(|id| format!(", inviting {id}"))
                        .unwrap_or_default(),
                ));
                if progress.running && ui.button("Cancel").clicked() {
                    app.send(Command::CancelMultiInvite);
                }
                ui.separator();
            }

            let mut online: Vec<&Friend> =
                state.friends.values().filter(|f| f.is_online()).collect();
            online.sort_by(|a, b| a.display_name.cmp(&b.display_name));
            if online.is_empty() {
                ui.label("No friends online.");
            } else {
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        for friend in online {
                            let mut selected = app.multi_invite_selected.contains(&friend.id);
                            if ui.checkbox(&mut selected, &friend.display_name).changed() {
                                if selected {
                                    app.multi_invite_selected.insert(friend.id.clone());
                                } else {
                                    app.multi_invite_selected.remove(&friend.id);
                                }
                            }
                        }
                    });
            }
            ui.add_space(4.0);
            let running = state.multi_invite.as_ref().is_some_and(|p| p.running);
            if ui
                .add_enabled(
                    !running && !app.multi_invite_selected.is_empty(),
                    egui::Button::new(format!(
                        "Invite {} selected",
                        app.multi_invite_selected.len()
                    )),
                )
                .clicked()
            {
                let user_ids: Vec<String> = app.multi_invite_selected.iter().cloned().collect();
                app.multi_invite_selected.clear();
                app.send(Command::MultiInvite {
                    user_ids,
                    instance_id: None,
                });
            }
            if !running && state.multi_invite.is_some() {
                ui.weak("Batch finished — see the friends' worlds dashboard.");
            }
        });
    if !open {
        app.multi_invite_open = false;
    }
}

/// The destructive-action confirmation dialog (unfriend / block).
pub(crate) fn confirm_dialog(app: &mut VrcNextApp, ui: &mut egui::Ui) {
    let Some(action) = app.confirm.clone() else {
        return;
    };
    let (title, message, command) = match &action {
        ConfirmAction::Unfriend { user_id } => (
            "Unfriend?",
            format!("Remove {user_id} from your friends?"),
            Command::Unfriend {
                user_id: user_id.clone(),
                confirm: true,
            },
        ),
        ConfirmAction::Block { user_id } => (
            "Block user?",
            format!("Block {user_id}? You will no longer see their content."),
            Command::ModerateUser {
                user_id: user_id.clone(),
                kind: PlayerModerationKind::Block,
                confirm: true,
            },
        ),
        ConfirmAction::ClearNotifications => (
            "Clear all notifications?",
            "Delete every notification?".to_string(),
            Command::ClearNotifications,
        ),
    };
    let mut open = true;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ui, |ui| {
            ui.label(message);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Confirm").clicked() {
                    app.send(command);
                    app.confirm = None;
                }
                if ui.button("Cancel").clicked() {
                    app.confirm = None;
                }
            });
        });
    if !open {
        app.confirm = None;
    }
}
