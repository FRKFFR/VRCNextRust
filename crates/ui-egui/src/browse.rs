//! M5 browse tabs: Worlds, Groups, Avatars and Favorites.
//!
//! Pure presentation like the rest of the UI (RULES.md §7): tabs read
//! [`AppState`] snapshots and push [`Command`]s — no I/O, no image loading.
//! Remote text (names, descriptions, posts) renders as plain labels,
//! never markup. Avatar switching goes through [`ConfirmAction`].

use eframe::egui;
use vrc_core::browse;
use vrc_core::{
    AppState, Avatar, Command, CreateInstanceRequest, DetailState, FavoriteGroup, FavoriteKind,
    Group, GroupAccessType, GroupInstance, GroupMember, GroupPost, InstanceKind, InstanceRegion,
    UserGroup, World,
};

use crate::{ConfirmAction, VrcNextApp, WorldsFeed};

// ---------------------------------------------------------------------------
// Pure label helpers (unit-tested below)
// ---------------------------------------------------------------------------

/// One-line summary for a world row: name — author — capacity — favorites.
pub(crate) fn world_label(world: &World) -> String {
    let mut label = world.name.clone();
    if label.is_empty() {
        label = world.id.clone();
    }
    let mut parts = Vec::new();
    if !world.author_name.is_empty() {
        parts.push(world.author_name.clone());
    }
    if world.capacity > 0 {
        parts.push(format!("capacity {}", world.capacity));
    }
    if world.favorites > 0 {
        parts.push(format!("{} favorites", world.favorites));
    }
    if parts.is_empty() {
        label
    } else {
        format!("{label} — {}", parts.join(" · "))
    }
}

/// One-line summary for a group row.
pub(crate) fn group_label(group: &Group) -> String {
    let name = if group.name.is_empty() {
        group.id.clone()
    } else {
        group.name.clone()
    };
    let mut label = name;
    if !group.short_code.is_empty() {
        label.push_str(&format!(" ({})", group.short_code));
    }
    if group.member_count > 0 || group.online_member_count > 0 {
        label.push_str(&format!(
            " — {} members, {} online",
            group.member_count, group.online_member_count
        ));
    }
    label
}

/// One-line summary for an avatar row.
pub(crate) fn avatar_label(avatar: &Avatar) -> String {
    let name = if avatar.name.is_empty() {
        avatar.id.clone()
    } else {
        avatar.name.clone()
    };
    let mut parts = Vec::new();
    if !avatar.author_name.is_empty() {
        parts.push(avatar.author_name.clone());
    }
    if !avatar.release_status.is_empty() {
        parts.push(avatar.release_status.clone());
    }
    if parts.is_empty() {
        name
    } else {
        format!("{name} — {}", parts.join(" · "))
    }
}

/// Display name for a group member (falls back to the user id).
pub(crate) fn member_label(member: &GroupMember) -> String {
    member
        .user
        .as_ref()
        .map(|u| u.display_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| member.user_id.clone())
}

/// One-line summary for a group post.
pub(crate) fn post_label(post: &GroupPost) -> String {
    if post.title.is_empty() {
        post.text.chars().take(80).collect()
    } else {
        format!(
            "{} — {}",
            post.title,
            post.text.chars().take(80).collect::<String>()
        )
    }
}

/// One-line summary for a group instance.
pub(crate) fn group_instance_label(instance: &GroupInstance) -> String {
    let world = if instance.world.name.is_empty() {
        instance.world.id.clone()
    } else {
        instance.world.name.clone()
    };
    if world.is_empty() {
        instance.location.clone()
    } else {
        format!(
            "{} · {} member{}",
            world,
            instance.member_count,
            if instance.member_count == 1 { "" } else { "s" }
        )
    }
}

/// `"Favorite"`/`"Unfavorite"` button label.
pub(crate) fn favorite_label(faved: bool) -> &'static str {
    if faved {
        "Unfavorite"
    } else {
        "Favorite"
    }
}

/// `"Join"`/`"Leave"` for a group based on the loaded membership.
pub(crate) fn group_action_label(group: &Group) -> &'static str {
    if group.my_member.is_some() {
        "Leave"
    } else {
        "Join"
    }
}

/// Display label for an instance kind (create-instance picker). The wire
/// `hidden`/`private` types are the user-facing Friends+/Invite(+) kinds.
pub(crate) fn instance_kind_label(kind: InstanceKind) -> &'static str {
    match kind {
        InstanceKind::Public => "Public",
        InstanceKind::Hidden => "Friends+",
        InstanceKind::Friends => "Friends",
        InstanceKind::Private => "Invite",
        InstanceKind::Group => "Group",
    }
}

/// Display label for an instance region (create-instance picker).
pub(crate) fn instance_region_label(region: InstanceRegion) -> &'static str {
    match region {
        InstanceRegion::Us => "US",
        InstanceRegion::Use => "US East",
        InstanceRegion::Eu => "Europe",
        InstanceRegion::Jp => "Japan",
        InstanceRegion::Unknown => "Any",
    }
}

/// Display label for a group access type (group-instance picker).
pub(crate) fn group_access_label(access: GroupAccessType) -> &'static str {
    match access {
        GroupAccessType::Public => "Public",
        GroupAccessType::Plus => "VRChat+",
        GroupAccessType::Members => "Members",
    }
}

/// One-line summary for an own-groups row (`GET /users/{me}/groups`).
pub(crate) fn user_group_label(group: &UserGroup) -> String {
    let name = if group.name.is_empty() {
        group.group_id.clone()
    } else {
        group.name.clone()
    };
    let mut label = name;
    if !group.short_code.is_empty() {
        label.push_str(&format!(" ({})", group.short_code));
    }
    if group.member_count > 0 {
        label.push_str(&format!(" — {} members", group.member_count));
    }
    if group.is_representing {
        label.push_str(" · representing");
    }
    label
}

/// One-line summary for a favorite-group set (`GET /favorite/groups`).
pub(crate) fn favorite_group_label(group: &FavoriteGroup) -> String {
    let name = if group.display_name.is_empty() {
        group.name.clone()
    } else {
        group.display_name.clone()
    };
    let name = if name.is_empty() {
        group.id.clone()
    } else {
        name
    };
    let mut label = name;
    if !group.kind.is_empty() {
        label.push_str(&format!(" — {}", group.kind));
    }
    if !group.owner_display_name.is_empty() {
        label.push_str(&format!(" · {}", group.owner_display_name));
    }
    label
}

/// The worlds-feed search command for the active tab feed.
pub(crate) fn worlds_command(app: &VrcNextApp, offset: usize) -> Command {
    let query = app.world_query.clone();
    match app.worlds_feed {
        WorldsFeed::Search => Command::SearchWorlds { query, offset },
        WorldsFeed::Active => Command::SearchActiveWorlds { query, offset },
        WorldsFeed::Recent => Command::SearchRecentWorlds { query, offset },
    }
}

/// The `kind`/`object_id` pair a favorite toggle sends.
fn favorite_command(state: &AppState, kind: FavoriteKind, object_id: &str) -> Option<Command> {
    if browse::is_favorite(state, kind, object_id) {
        Some(Command::RemoveFavorite {
            kind,
            object_id: object_id.to_string(),
        })
    } else {
        Some(Command::AddFavorite {
            kind,
            favorite_id: object_id.to_string(),
            tags: Vec::new(),
        })
    }
}

/// Small favorite/unfavorite toggle for a world/avatar row.
fn favorite_button(
    app: &mut VrcNextApp,
    ui: &mut egui::Ui,
    state: &AppState,
    kind: FavoriteKind,
    object_id: &str,
) {
    let faved = browse::is_favorite(state, kind, object_id);
    if ui
        .small_button(favorite_label(faved))
        .on_hover_text("Toggle favorite")
        .clicked()
    {
        if let Some(command) = favorite_command(state, kind, object_id) {
            app.send(command);
        }
    }
}

/// "More" button for a [`BrowsePage`]; loads the next offset page.
fn more_button<T>(
    app: &mut VrcNextApp,
    ui: &mut egui::Ui,
    page: &vrc_core::BrowsePage<T>,
    load: impl FnOnce(&mut VrcNextApp, usize),
) {
    if browse::page_has_more(page)
        && ui
            .small_button("More")
            .on_hover_text("Load the next page")
            .clicked()
    {
        load(app, page.offset + page.items.len());
    }
}

// ---------------------------------------------------------------------------
// Worlds tab
// ---------------------------------------------------------------------------

pub(crate) fn worlds_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    ui.horizontal(|ui| {
        for (feed, label) in [
            (WorldsFeed::Search, "Search"),
            (WorldsFeed::Active, "Active"),
            (WorldsFeed::Recent, "Recent"),
        ] {
            if ui
                .selectable_value(&mut app.worlds_feed, feed, label)
                .changed()
            {
                app.send(worlds_command(app, 0));
            }
        }
        ui.separator();
        if search_bar(ui, &mut app.world_query, "Filter worlds…") {
            app.send(worlds_command(app, 0));
        }
        if ui.button("Refresh").clicked() {
            app.send(worlds_command(app, 0));
        }
    });
    ui.separator();

    if let Some(id) = app.world_detail.clone() {
        match state.world_details.get(&id) {
            Some(DetailState::Known(world)) => world_detail_card(app, ui, state, world),
            Some(DetailState::Pending) => {
                ui.label("Loading world details…");
            }
            Some(DetailState::Unknown { .. }) => {
                ui.label("World details unavailable.");
            }
            None => {}
        }
        ui.separator();
    }

    let page = match app.worlds_feed {
        WorldsFeed::Search => state.world_search.as_ref(),
        WorldsFeed::Active => state.active_worlds.as_ref(),
        WorldsFeed::Recent => state.recent_worlds.as_ref(),
    };
    let Some(page) = page else {
        ui.label("Pick a feed above.");
        return;
    };
    if page.loading {
        ui.label("Loading…");
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for world in &page.items {
            world_row(app, ui, state, world);
        }
    });
    more_button(app, ui, page, |app, offset| {
        app.send(worlds_command(app, offset));
    });
}

fn world_row(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState, world: &World) {
    ui.horizontal(|ui| {
        if ui
            .strong(world_label(world))
            .on_hover_text("World details")
            .clicked()
        {
            app.world_detail = Some(world.id.clone());
            app.send(Command::LoadWorld {
                world_id: world.id.clone(),
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            favorite_button(app, ui, state, FavoriteKind::World, &world.id);
        });
    });
    let tags = world_tags(world);
    if !tags.is_empty() {
        ui.weak(tags);
    }
}

/// Detail card for a loaded world: metadata + instance creation and
/// self-invite for the instance created here.
fn world_detail_card(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState, world: &World) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.strong(if world.name.is_empty() {
                world.id.clone()
            } else {
                world.name.clone()
            });
            ui.small(egui::RichText::new(world.id.clone()).monospace().weak());
            let mut meta = Vec::new();
            if !world.author_name.is_empty() {
                meta.push(format!("by {}", world.author_name));
            }
            if world.capacity > 0 {
                meta.push(format!("capacity {}", world.capacity));
            }
            if world.favorites > 0 {
                meta.push(format!("{} favorites", world.favorites));
            }
            if !world.release_status.is_empty() {
                meta.push(world.release_status.clone());
            }
            if !meta.is_empty() {
                ui.weak(meta.join(" · "));
            }
            if !world.description.is_empty() {
                ui.label(world.description.lines().next().unwrap_or_default());
            }
            if !world.url.is_empty() {
                ui.weak(world.url.clone());
            }
            ui.separator();
            ui.horizontal(|ui| {
                egui::ComboBox::from_label("Type")
                    .selected_text(instance_kind_label(app.new_instance_kind))
                    .show_ui(ui, |ui| {
                        for kind in [
                            InstanceKind::Public,
                            InstanceKind::Hidden,
                            InstanceKind::Friends,
                            InstanceKind::Private,
                            InstanceKind::Group,
                        ] {
                            ui.selectable_value(
                                &mut app.new_instance_kind,
                                kind,
                                instance_kind_label(kind),
                            );
                        }
                    });
                egui::ComboBox::from_label("Region")
                    .selected_text(instance_region_label(app.new_instance_region))
                    .show_ui(ui, |ui| {
                        for region in [
                            InstanceRegion::Us,
                            InstanceRegion::Use,
                            InstanceRegion::Eu,
                            InstanceRegion::Jp,
                            InstanceRegion::Unknown,
                        ] {
                            ui.selectable_value(
                                &mut app.new_instance_region,
                                region,
                                instance_region_label(region),
                            );
                        }
                    });
                if app.new_instance_kind == InstanceKind::Group {
                    ui.add(
                        egui::TextEdit::singleline(&mut app.new_instance_group)
                            .hint_text("grp_… owner id")
                            .desired_width(140.0),
                    );
                    egui::ComboBox::from_label("Access")
                        .selected_text(group_access_label(app.new_instance_access))
                        .show_ui(ui, |ui| {
                            for access in [
                                GroupAccessType::Public,
                                GroupAccessType::Plus,
                                GroupAccessType::Members,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_instance_access,
                                    access,
                                    group_access_label(access),
                                );
                            }
                        });
                }
                if app.new_instance_kind == InstanceKind::Private {
                    ui.checkbox(&mut app.new_instance_can_request_invite, "Invite+")
                        .on_hover_text("Invite+: others may request an invite");
                }
                if ui.button("Create instance").clicked() {
                    let mut request = CreateInstanceRequest::new(
                        world.id.clone(),
                        app.new_instance_kind,
                        app.new_instance_region,
                    );
                    if app.new_instance_kind == InstanceKind::Group {
                        let group_id = app.new_instance_group.trim().to_string();
                        if !group_id.is_empty() {
                            request.owner_id = Some(group_id);
                            request.group_access_type = Some(app.new_instance_access);
                        }
                    }
                    if app.new_instance_kind == InstanceKind::Private {
                        request.can_request_invite = Some(app.new_instance_can_request_invite);
                    }
                    app.send(Command::CreateInstance { request });
                }
            });
            // The instance created (or loaded) for this world: show it and
            // offer self-invite without leaving the detail panel.
            if let Some(instance) = &state.instance {
                if instance.world_id == world.id && !instance.instance_id.is_empty() {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(format!("Instance: {}", instance.instance_id));
                        if !instance.location.is_empty() {
                            ui.weak(instance.location.clone());
                        }
                        if ui.button("Invite myself").clicked() {
                            app.send(Command::InviteMyselfToInstance {
                                world_id: instance.world_id.clone(),
                                instance_id: instance.instance_id.clone(),
                            });
                        }
                    });
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Groups tab
// ---------------------------------------------------------------------------

pub(crate) fn groups_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    ui.horizontal(|ui| {
        let mut mine = app.groups_mine;
        ui.selectable_value(&mut mine, false, "Search");
        ui.selectable_value(&mut mine, true, "My groups");
        if mine != app.groups_mine {
            app.groups_mine = mine;
            if mine {
                app.send(Command::LoadOwnGroups);
            }
        }
        ui.separator();
        if !app.groups_mine {
            if search_bar(ui, &mut app.group_query, "Search groups…") {
                app.send(Command::SearchGroups {
                    query: app.group_query.clone(),
                    offset: 0,
                });
            }
            if ui.button("Refresh").clicked() {
                app.send(Command::SearchGroups {
                    query: app.group_query.clone(),
                    offset: 0,
                });
            }
        } else if ui.button("Refresh").clicked() {
            app.send(Command::LoadOwnGroups);
        }
    });
    ui.separator();

    if let Some(id) = app.group_detail.clone() {
        match state.group_details.get(&id) {
            Some(DetailState::Known(group)) => group_detail_card(app, ui, group),
            Some(DetailState::Pending) => {
                ui.label("Loading group details…");
            }
            Some(DetailState::Unknown { .. }) => {
                ui.label("Group details unavailable.");
            }
            None => {}
        }
        ui.separator();
        group_subsections(app, ui, state, &id);
        ui.separator();
    }

    if app.groups_mine {
        own_groups_list(app, ui, state);
        return;
    }

    let Some(page) = &state.group_search else {
        ui.label("Search for groups above.");
        return;
    };
    if page.loading {
        ui.label("Searching…");
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for group in &page.items {
            group_row(app, ui, group);
        }
    });
    more_button(app, ui, page, |app, offset| {
        app.send(Command::SearchGroups {
            query: app.group_query.clone(),
            offset,
        });
    });
}

/// The "My groups" list (`GET /users/{me}/groups` — unpaged).
fn own_groups_list(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    let Some(groups) = &state.own_groups else {
        ui.label("Your groups are not loaded yet.");
        return;
    };
    if groups.is_empty() {
        ui.label("You are not a member of any public groups.");
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for group in groups {
            ui.horizontal(|ui| {
                if ui
                    .strong(user_group_label(group))
                    .on_hover_text("Group details")
                    .clicked()
                {
                    app.group_detail = Some(group.group_id.clone());
                    app.send(Command::LoadGroup {
                        group_id: group.group_id.clone(),
                        include_roles: true,
                    });
                }
            });
            ui.add_space(2.0);
        }
    });
}

fn group_row(app: &mut VrcNextApp, ui: &mut egui::Ui, group: &Group) {
    ui.horizontal(|ui| {
        if ui
            .strong(group_label(group))
            .on_hover_text("Group details")
            .clicked()
        {
            app.group_detail = Some(group.id.clone());
            app.send(Command::LoadGroup {
                group_id: group.id.clone(),
                include_roles: true,
            });
        }
    });
    ui.add_space(2.0);
}

/// Detail card for a loaded group: metadata + join/leave.
fn group_detail_card(app: &mut VrcNextApp, ui: &mut egui::Ui, group: &Group) {
    let group_id = group.id.clone();
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.strong(if group.name.is_empty() {
                group.id.clone()
            } else {
                group.name.clone()
            });
            if !group.short_code.is_empty() {
                ui.label(format!("Code: {}", group.short_code));
            }
            ui.small(egui::RichText::new(group.id.clone()).monospace().weak());
            ui.weak(format!(
                "{} members · {} online",
                group.member_count, group.online_member_count
            ));
            if !group.description.is_empty() {
                ui.label(group.description.lines().next().unwrap_or_default());
            }
            ui.separator();
            ui.horizontal(|ui| {
                let member = group.my_member.is_some();
                if ui
                    .button(group_action_label(group))
                    .on_hover_text(if member {
                        "Leave this group"
                    } else {
                        "Join this group"
                    })
                    .clicked()
                {
                    if member {
                        app.send(Command::LeaveGroup {
                            group_id: group_id.clone(),
                        });
                    } else {
                        app.send(Command::JoinGroup {
                            group_id: group_id.clone(),
                        });
                    }
                }
                ui.separator();
                for (gallery_id, name) in group
                    .galleries
                    .iter()
                    .map(|g| (g.id.clone(), g.name.clone()))
                {
                    if ui
                        .small_button(if name.is_empty() {
                            "Gallery".to_string()
                        } else {
                            name
                        })
                        .on_hover_text("Load gallery images")
                        .clicked()
                    {
                        app.send(Command::LoadGroupGalleryImages {
                            group_id: group_id.clone(),
                            gallery_id,
                            offset: 0,
                            approved: None,
                        });
                    }
                }
            });
        });
}

/// Members / posts / instances sections under the open group detail.
fn group_subsections(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState, group_id: &str) {
    ui.horizontal(|ui| {
        if ui.button("Members").clicked() {
            app.send(Command::LoadGroupMembers {
                group_id: group_id.to_string(),
                offset: 0,
            });
        }
        if ui.button("Posts").clicked() {
            app.send(Command::LoadGroupPosts {
                group_id: group_id.to_string(),
                offset: 0,
                public_only: false,
            });
        }
        if ui.button("Instances").clicked() {
            app.send(Command::LoadGroupInstances {
                group_id: group_id.to_string(),
            });
        }
    });

    if let Some(page) = state.group_members.get(group_id) {
        if !page.items.is_empty() {
            ui.strong("Members");
            for member in &page.items {
                ui.label(format!("• {}", member_label(member)));
            }
        }
        more_button(app, ui, page, |app, offset| {
            app.send(Command::LoadGroupMembers {
                group_id: group_id.to_string(),
                offset,
            });
        });
    }
    if let Some(page) = state.group_posts.get(group_id) {
        if !page.items.is_empty() {
            ui.strong("Posts");
            for post in &page.items {
                ui.label(format!("• {}", post_label(post)));
            }
        }
        more_button(app, ui, page, |app, offset| {
            app.send(Command::LoadGroupPosts {
                group_id: group_id.to_string(),
                offset,
                public_only: false,
            });
        });
    }
    if let Some(page) = state.group_instances.get(group_id) {
        if !page.items.is_empty() {
            ui.strong("Instances");
            for instance in &page.items {
                ui.label(format!("• {}", group_instance_label(instance)));
            }
        }
    }
    // Gallery images render under their `group:gallery` keys.
    let prefix = format!("{group_id}:");
    for (key, page) in state.group_gallery_images.iter() {
        if !key.starts_with(&prefix) || page.items.is_empty() {
            continue;
        }
        ui.strong(format!("Gallery {}", &key[prefix.len()..]));
        for image in &page.items {
            ui.horizontal(|ui| {
                ui.label(format!("• {}", image.file_id));
                if !image.image_url.is_empty() {
                    ui.weak(image.image_url.clone());
                }
            });
        }
        if let Some((gid, gal)) = browse::parse_gallery_key(key) {
            more_button(app, ui, page, |app, offset| {
                app.send(Command::LoadGroupGalleryImages {
                    group_id: gid.to_string(),
                    gallery_id: gal.to_string(),
                    offset,
                    approved: None,
                });
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Avatars tab
// ---------------------------------------------------------------------------

pub(crate) fn avatars_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    ui.horizontal(|ui| {
        let mut own = app.avatar_own;
        ui.selectable_value(&mut own, false, "Featured");
        ui.selectable_value(&mut own, true, "My avatars");
        if own != app.avatar_own {
            app.avatar_own = own;
            app.send(Command::LoadAvatars { offset: 0, own });
        }
        ui.separator();
        if ui.button("Current").clicked() {
            app.send(Command::LoadOwnAvatar);
        }
        if ui.button("Refresh").clicked() {
            app.send(Command::LoadAvatars {
                offset: 0,
                own: app.avatar_own,
            });
        }
    });
    ui.separator();

    if let Some(avatar) = &state.own_avatar {
        ui.horizontal(|ui| {
            ui.strong(format!("Current: {}", avatar_label(avatar)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                favorite_button(app, ui, state, FavoriteKind::Avatar, &avatar.id);
            });
        });
        ui.separator();
    }

    if let Some(id) = app.avatar_detail.clone() {
        match state.avatar_details.get(&id) {
            Some(DetailState::Known(avatar)) => avatar_detail_card(app, ui, state, avatar),
            Some(DetailState::Pending) => {
                ui.label("Loading avatar details…");
            }
            Some(DetailState::Unknown { .. }) => {
                ui.label("Avatar details unavailable.");
            }
            None => {}
        }
        ui.separator();
    }

    let Some(page) = &state.avatar_search else {
        ui.label("Search for avatars above.");
        return;
    };
    if page.loading {
        ui.label("Loading…");
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for avatar in &page.items {
            avatar_row(app, ui, state, avatar);
        }
    });
    more_button(app, ui, page, |app, offset| {
        app.send(Command::LoadAvatars {
            offset,
            own: app.avatar_own,
        });
    });
}

fn avatar_row(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState, avatar: &Avatar) {
    ui.horizontal(|ui| {
        if ui
            .strong(avatar_label(avatar))
            .on_hover_text("Avatar details")
            .clicked()
        {
            app.avatar_detail = Some(avatar.id.clone());
            app.send(Command::LoadAvatar {
                avatar_id: avatar.id.clone(),
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            favorite_button(app, ui, state, FavoriteKind::Avatar, &avatar.id);
            if ui
                .small_button("Select")
                .on_hover_text("Switch into this avatar")
                .clicked()
            {
                app.confirm = Some(ConfirmAction::SelectAvatar {
                    avatar_id: avatar.id.clone(),
                    name: avatar.name.clone(),
                });
            }
        });
    });
}

/// Detail card for a loaded avatar with the guarded select action.
fn avatar_detail_card(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState, avatar: &Avatar) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.strong(if avatar.name.is_empty() {
                avatar.id.clone()
            } else {
                avatar.name.clone()
            });
            ui.small(egui::RichText::new(avatar.id.clone()).monospace().weak());
            let mut meta = Vec::new();
            if !avatar.author_name.is_empty() {
                meta.push(format!("by {}", avatar.author_name));
            }
            if !avatar.release_status.is_empty() {
                meta.push(avatar.release_status.clone());
            }
            if avatar.version > 0 {
                meta.push(format!("v{}", avatar.version));
            }
            if !meta.is_empty() {
                ui.weak(meta.join(" · "));
            }
            if !avatar.description.is_empty() {
                ui.label(avatar.description.lines().next().unwrap_or_default());
            }
            ui.separator();
            ui.horizontal(|ui| {
                favorite_button(app, ui, state, FavoriteKind::Avatar, &avatar.id);
                if ui
                    .button("Select avatar")
                    .on_hover_text("Switch into this avatar")
                    .clicked()
                {
                    app.confirm = Some(ConfirmAction::SelectAvatar {
                        avatar_id: avatar.id.clone(),
                        name: avatar.name.clone(),
                    });
                }
            });
        });
}

// ---------------------------------------------------------------------------
// Favorites tab
// ---------------------------------------------------------------------------

pub(crate) fn favorites_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &AppState) {
    ui.horizontal(|ui| {
        if ui.button("Refresh").clicked() {
            app.send(Command::RefreshFavorites);
            app.send(Command::LoadFavoriteGroups { offset: 0 });
        }
        ui.weak(format!(
            "{} favorite{}",
            state.favorite_records.len(),
            if state.favorite_records.len() == 1 {
                ""
            } else {
                "s"
            }
        ));
    });
    ui.separator();

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.strong("Worlds");
            ui.separator();
            match &state.favorite_worlds {
                Some(page) => {
                    for world in &page.items {
                        ui.horizontal(|ui| {
                            ui.label(world_label(world));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Remove").clicked() {
                                        app.send(Command::RemoveFavorite {
                                            kind: FavoriteKind::World,
                                            object_id: world.id.clone(),
                                        });
                                    }
                                },
                            );
                        });
                    }
                    more_button(app, ui, page, |app, offset| {
                        app.send(Command::LoadFavoriteWorlds { offset });
                    });
                }
                None => {
                    ui.weak("Not loaded — press Refresh.");
                }
            }
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.strong("Avatars");
            ui.separator();
            match &state.favorite_avatars {
                Some(page) => {
                    for avatar in &page.items {
                        ui.horizontal(|ui| {
                            ui.label(avatar_label(avatar));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Remove").clicked() {
                                        app.send(Command::RemoveFavorite {
                                            kind: FavoriteKind::Avatar,
                                            object_id: avatar.id.clone(),
                                        });
                                    }
                                },
                            );
                        });
                    }
                    more_button(app, ui, page, |app, offset| {
                        app.send(Command::LoadFavoriteAvatars { offset });
                    });
                }
                None => {
                    ui.weak("Not loaded — press Refresh.");
                }
            }
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.strong("Favorite groups");
            ui.separator();
            match &state.favorite_groups {
                Some(page) => {
                    for group in &page.items {
                        ui.label(favorite_group_label(group));
                    }
                    more_button(app, ui, page, |app, offset| {
                        app.send(Command::LoadFavoriteGroups { offset });
                    });
                }
                None => {
                    ui.weak("Not loaded — press Refresh.");
                }
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Shared bits
// ---------------------------------------------------------------------------

/// Live search box; returns `true` when the text changed so the caller can
/// re-queue the (debounced) search command.
fn search_bar(ui: &mut egui::Ui, query: &mut String, hint: &str) -> bool {
    egui::TextEdit::singleline(query)
        .hint_text(hint)
        .desired_width(280.0)
        .show(ui)
        .response
        .changed()
}

/// Platform preview line for a world row.
pub(crate) fn world_tags(world: &World) -> String {
    world.platforms.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vrc_core::{AppState, Favorite};

    #[test]
    fn world_label_uses_name_and_metadata() {
        let world = World {
            id: "wrld_1".to_string(),
            name: "Test World".to_string(),
            author_name: "author".to_string(),
            capacity: 20,
            favorites: 3,
            ..World::default()
        };
        let label = world_label(&world);
        assert!(label.contains("Test World"));
        assert!(label.contains("author"));
        assert!(label.contains("capacity 20"));
    }

    #[test]
    fn world_label_falls_back_to_id() {
        let world = World {
            id: "wrld_x".to_string(),
            ..World::default()
        };
        assert_eq!(world_label(&world), "wrld_x");
    }

    #[test]
    fn group_label_includes_code_and_counts() {
        let group = Group {
            id: "grp_1".to_string(),
            name: "My Group".to_string(),
            short_code: "abcd".to_string(),
            member_count: 10,
            online_member_count: 4,
            ..Group::default()
        };
        let label = group_label(&group);
        assert!(label.contains("My Group"));
        assert!(label.contains("(abcd)"));
        assert!(label.contains("10 members"));
    }

    #[test]
    fn avatar_label_uses_name_and_author() {
        let avatar = Avatar {
            id: "avtr_1".to_string(),
            name: "Cool Avatar".to_string(),
            author_name: "maker".to_string(),
            release_status: "public".to_string(),
            ..Avatar::default()
        };
        let label = avatar_label(&avatar);
        assert!(label.contains("Cool Avatar"));
        assert!(label.contains("maker"));
    }

    #[test]
    fn member_label_prefers_display_name() {
        let member = GroupMember {
            user_id: "usr_1".to_string(),
            user: Some(vrc_core::GroupMemberUser {
                display_name: "Alice".to_string(),
                ..Default::default()
            }),
            ..GroupMember::default()
        };
        assert_eq!(member_label(&member), "Alice");
        let bare = GroupMember {
            user_id: "usr_2".to_string(),
            ..GroupMember::default()
        };
        assert_eq!(member_label(&bare), "usr_2");
    }

    #[test]
    fn group_action_label_reflects_membership() {
        let mut group = Group::default();
        assert_eq!(group_action_label(&group), "Join");
        group.my_member = Some(GroupMember::default());
        assert_eq!(group_action_label(&group), "Leave");
    }

    #[test]
    fn favorite_command_adds_and_removes() {
        let mut state = AppState::default();
        state.favorite_records.push(Favorite {
            id: "fvrt_1".to_string(),
            kind: "world".to_string(),
            favorite_id: "wrld_1".to_string(),
            ..Favorite::default()
        });
        match favorite_command(&state, FavoriteKind::World, "wrld_1") {
            Some(Command::RemoveFavorite { kind, object_id }) => {
                assert_eq!(kind, FavoriteKind::World);
                assert_eq!(object_id, "wrld_1");
            }
            _ => panic!("expected RemoveFavorite"),
        }
        match favorite_command(&state, FavoriteKind::Avatar, "avtr_1") {
            Some(Command::AddFavorite {
                kind, favorite_id, ..
            }) => {
                assert_eq!(kind, FavoriteKind::Avatar);
                assert_eq!(favorite_id, "avtr_1");
            }
            _ => panic!("expected AddFavorite"),
        }
    }

    #[test]
    fn instance_labels_cover_all_kinds() {
        // Wire `hidden`/`private` surface as the user-facing Friends+/Invite.
        assert_eq!(instance_kind_label(InstanceKind::Public), "Public");
        assert_eq!(instance_kind_label(InstanceKind::Hidden), "Friends+");
        assert_eq!(instance_kind_label(InstanceKind::Friends), "Friends");
        assert_eq!(instance_kind_label(InstanceKind::Private), "Invite");
        assert_eq!(instance_kind_label(InstanceKind::Group), "Group");
        for region in [
            InstanceRegion::Us,
            InstanceRegion::Use,
            InstanceRegion::Eu,
            InstanceRegion::Jp,
            InstanceRegion::Unknown,
        ] {
            assert!(!instance_region_label(region).is_empty());
        }
    }
}
