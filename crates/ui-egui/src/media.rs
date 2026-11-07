//! M6 Media tab: local photo library grid, folder management, favorites,
//! blur-until-hover, metadata panel and the Discord webhook relay UI.
//!
//! Pure presentation like the rest of the UI (RULES.md §7): the tab reads
//! [`AppState`] snapshots and pushes [`Command`]s — thumbnails are decoded
//! by `vrc-core` and arrive as raw RGBA through [`ThumbnailHandle`]; this
//! module only uploads them to egui textures.

use eframe::egui;
use secrecy::SecretString;
use vrc_core::{format, Command, MediaFilter, MediaItem, ThumbSource};

use crate::VrcNextApp;

/// Cell size for one grid tile (image + two label lines).
const CELL_W: f32 = 168.0;
const CELL_IMG: f32 = 150.0;

impl VrcNextApp {
    /// The media page filter built from the tab's current controls.
    pub(crate) fn media_filter(&self, offset: usize) -> MediaFilter {
        MediaFilter {
            folder: self.media_folder.clone(),
            favorites_only: self.media_favorites_only,
            query: non_empty(&self.media_query),
            offset: offset as u32,
            ..MediaFilter::default()
        }
    }

    /// (Re)load the first media page for the current filters.
    pub(crate) fn reload_media(&self) {
        self.send(Command::LoadMedia {
            filter: self.media_filter(0),
        });
    }

    /// Look up (or lazily create) the egui texture for a thumbnail key.
    /// Decoding happens in `vrc-core`; this only uploads finished RGBA.
    pub(crate) fn thumb_texture(
        &mut self,
        ui: &egui::Ui,
        source: &ThumbSource,
    ) -> Option<egui::TextureHandle> {
        let key = source.key();
        if let Some(tex) = self.tex.get(&key) {
            return Some(tex.clone());
        }
        if let Some(image) = self.thumbnails.get(&key) {
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [image.width as usize, image.height as usize],
                &image.rgba,
            );
            let tex = ui
                .ctx()
                .load_texture(&key, color, egui::TextureOptions::LINEAR);
            // Bound the GPU-side cache: at the cap, drop every texture —
            // re-upload is cheap because the RGBA stays in the core LRU.
            if self.tex.len() >= 1024 {
                self.tex.clear();
            }
            self.tex.insert(key.clone(), tex.clone());
            return Some(tex);
        }
        // Not decoded yet — claim the decode once per session; failures
        // are negatively cached core-side so this never hot-loops.
        if self.thumb_requested.insert(key.clone()) {
            self.send(Command::RequestThumbnail {
                key,
                source: source.clone(),
            });
        }
        None
    }
}

/// Paint a `size`×`size` thumbnail box: the decoded texture (aspect
/// preserved, centered) when ready, a dim placeholder otherwise. Used by
/// the browse-tab cards; returns the painted rect.
pub(crate) fn thumb_box(
    app: &mut VrcNextApp,
    ui: &mut egui::Ui,
    source: &ThumbSource,
    size: f32,
) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    match app.thumb_texture(ui, source) {
        Some(tex) => {
            let ts = tex.size_vec2();
            let scale = (size / ts.x).min(size / ts.y);
            let draw = ts * scale;
            let at = rect.center() - draw / 2.0;
            ui.put(
                egui::Rect::from_min_size(at, draw),
                egui::Image::new((tex.id(), ts)).fit_to_exact_size(draw),
            );
        }
        None => {
            ui.painter()
                .rect_filled(rect, 4.0, egui::Color32::from_gray(40));
        }
    }
    rect
}

fn non_empty(query: &str) -> Option<String> {
    let trimmed = query.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

// ---------------------------------------------------------------------------
// Media tab
// ---------------------------------------------------------------------------

pub(crate) fn media_tab(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &vrc_core::AppState) {
    filter_bar(app, ui, state);
    ui.separator();
    folders_row(app, ui, state);

    let Some(page) = &state.media else {
        ui.label("No photos indexed yet — press Scan to index your folders.");
        webhooks_section(app, ui, state);
        return;
    };
    if page.loading && page.items.is_empty() {
        ui.label("Loading…");
    }

    let columns = ((ui.available_width() / (CELL_W + 8.0)).floor() as usize).max(1);
    media_grid(app, ui, state, page, columns);
    infinite_scroll_sentinel(app, ui, page);

    if let Some(id) = app.media_selected {
        if let Some(item) = page.items.iter().find(|i| i.id == id) {
            media_detail_window(app, ui, item);
        } else {
            app.media_selected = None;
        }
    }

    webhooks_section(app, ui, state);
}

fn filter_bar(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &vrc_core::AppState) {
    ui.horizontal(|ui| {
        let folder_label = app
            .media_folder
            .clone()
            .unwrap_or_else(|| "All folders".to_string());
        egui::ComboBox::from_label("Folder")
            .selected_text(folder_label)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(app.media_folder.is_none(), "All folders")
                    .clicked()
                {
                    app.media_folder = None;
                    app.reload_media();
                }
                for folder in &state.media_folders {
                    if ui
                        .selectable_label(
                            app.media_folder.as_deref() == Some(folder.path.as_str()),
                            &folder.path,
                        )
                        .clicked()
                    {
                        app.media_folder = Some(folder.path.clone());
                        app.reload_media();
                    }
                }
            });
        if ui
            .checkbox(&mut app.media_favorites_only, "Favorites only")
            .changed()
        {
            app.reload_media();
        }
        ui.separator();
        let query = egui::TextEdit::singleline(&mut app.media_query)
            .hint_text("Filter path / world…")
            .desired_width(200.0)
            .show(ui);
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (enter && query.response.has_focus()) || ui.button("Apply").clicked() {
            app.reload_media();
        }
        ui.separator();
        let scanning = state.media_scanning;
        if ui
            .add_enabled(!scanning, egui::Button::new("Scan"))
            .on_hover_text("Rescan all configured folders")
            .clicked()
        {
            app.send(Command::ScanMedia);
        }
        if scanning {
            ui.weak("scanning…");
        } else if let Some(scan) = &state.media_last_scan {
            ui.weak(format!(
                "{} files · {} upserted · {} removed",
                scan.files, scan.upserted, scan.removed
            ));
        }
        if ui.button("Refresh").clicked() {
            app.reload_media();
        }
        let mut blur = state.media_blur;
        if ui
            .checkbox(&mut blur, "Blur until hover")
            .on_hover_text("Persisted; hides thumbnails until the pointer is over them")
            .changed()
        {
            app.send(Command::SetMediaBlurUntilHover { enabled: blur });
        }
    });
}

/// Scan-folder management row (custom folders can be added/removed).
fn folders_row(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &vrc_core::AppState) {
    egui::CollapsingHeader::new(format!("Folders ({})", state.media_folders.len()))
        .default_open(false)
        .show(ui, |ui| {
            for folder in &state.media_folders {
                ui.horizontal(|ui| {
                    ui.label(&folder.path);
                    if folder.custom {
                        ui.weak("custom");
                        if ui.small_button("Remove").clicked() {
                            app.send(Command::RemoveMediaFolder {
                                path: folder.path.clone(),
                            });
                        }
                    } else {
                        ui.weak("default");
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut app.media_folder_input)
                        .hint_text("Add folder path…")
                        .desired_width(280.0),
                );
                if ui.button("Add").clicked() && !app.media_folder_input.trim().is_empty() {
                    let path = app.media_folder_input.trim().to_string();
                    app.media_folder_input.clear();
                    app.send(Command::AddMediaFolder { path });
                }
            });
        });
    ui.add_space(4.0);
}

/// The thumbnail grid, `columns` tiles per row.
fn media_grid(
    app: &mut VrcNextApp,
    ui: &mut egui::Ui,
    state: &vrc_core::AppState,
    page: &vrc_core::BrowsePage<MediaItem>,
    columns: usize,
) {
    for chunk in page.items.chunks(columns) {
        ui.horizontal(|ui| {
            for item in chunk {
                media_cell(app, ui, state, item);
                ui.add_space(8.0);
            }
        });
        ui.add_space(4.0);
    }
}

/// One grid tile: thumbnail (blurred until hover when enabled), file
/// name, date and a favorite toggle.
fn media_cell(
    app: &mut VrcNextApp,
    ui: &mut egui::Ui,
    state: &vrc_core::AppState,
    item: &MediaItem,
) {
    ui.vertical(|ui| {
        ui.set_min_width(CELL_W);
        let (img_rect, response) =
            ui.allocate_exact_size(egui::vec2(CELL_W, CELL_IMG), egui::Sense::click());
        if response.clicked() {
            app.media_selected = Some(item.id);
        }
        let hovered = response.hovered();

        let blurred = state.media_blur && !hovered;
        if blurred {
            ui.painter()
                .rect_filled(img_rect, 4.0, egui::Color32::from_gray(28));
            ui.painter().text(
                img_rect.center(),
                egui::Align2::CENTER_CENTER,
                "hover to reveal",
                egui::FontId::proportional(12.0),
                egui::Color32::GRAY,
            );
        } else {
            let source = ThumbSource::File(item.path.clone());
            match app.thumb_texture(ui, &source) {
                Some(tex) => {
                    let ts = tex.size_vec2();
                    let scale = (CELL_W / ts.x).min(CELL_IMG / ts.y);
                    let draw = ts * scale;
                    let at = img_rect.center() - draw / 2.0;
                    ui.put(
                        egui::Rect::from_min_size(at, draw),
                        egui::Image::new((tex.id(), ts)).fit_to_exact_size(draw),
                    );
                }
                None => {
                    ui.painter()
                        .rect_filled(img_rect, 4.0, egui::Color32::from_gray(40));
                    if item.width > 0 {
                        ui.painter().text(
                            img_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("{}×{}", item.width, item.height),
                            egui::FontId::proportional(11.0),
                            egui::Color32::DARK_GRAY,
                        );
                    }
                }
            }
        }
        ui.painter().rect_stroke(
            img_rect,
            4.0,
            egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
            egui::StrokeKind::Inside,
        );

        // Labels.
        ui.horizontal(|ui| {
            let name = item.path.rsplit(['/', '\\']).next().unwrap_or(&item.path);
            ui.add(egui::Label::new(egui::RichText::new(name).small().strong()).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(if item.is_favorite { "Unfav" } else { "Fav" })
                    .on_hover_text("Toggle favorite")
                    .clicked()
                {
                    app.send(Command::SetMediaFavorite {
                        id: item.id,
                        favorite: !item.is_favorite,
                    });
                }
            });
        });
        ui.label(
            egui::RichText::new(format::fmt_ts(item.taken_at))
                .small()
                .weak(),
        );
        if !item.world_name.is_empty() {
            ui.add(
                egui::Label::new(egui::RichText::new(&item.world_name).small().weak()).truncate(),
            );
        }
    });
}

/// Fetch the next page when the sentinel scrolls into view.
fn infinite_scroll_sentinel(
    app: &mut VrcNextApp,
    ui: &mut egui::Ui,
    page: &vrc_core::BrowsePage<MediaItem>,
) {
    if !page.has_more() || page.loading {
        return;
    }
    let next = page.offset + page.items.len();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 1.0), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        app.send(Command::LoadMedia {
            filter: app.media_filter(next),
        });
    }
}

/// Floating detail window for the selected photo.
fn media_detail_window(app: &mut VrcNextApp, ui: &mut egui::Ui, item: &MediaItem) {
    let mut open = true;
    egui::Window::new("Photo")
        .open(&mut open)
        .default_width(420.0)
        .show(ui, |ui| {
            let source = ThumbSource::File(item.path.clone());
            if let Some(tex) = app.thumb_texture(ui, &source) {
                let size = tex.size_vec2();
                let max = egui::vec2(380.0, 300.0);
                let scale = (max.x / size.x).min(max.y / size.y).min(1.0);
                let draw = size * scale;
                ui.add(egui::Image::new((tex.id(), tex.size_vec2())).fit_to_exact_size(draw));
            } else {
                ui.label("Loading thumbnail…");
            }
            ui.separator();
            ui.monospace(&item.path);
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{}×{} · {}",
                    item.width,
                    item.height,
                    fmt_bytes(item.size)
                ));
                ui.label(format::fmt_ts(item.taken_at));
            });
            if !item.world_name.is_empty() || !item.world_id.is_empty() {
                ui.label(format!(
                    "World: {}",
                    if item.world_name.is_empty() {
                        item.world_id.as_str()
                    } else {
                        item.world_name.as_str()
                    }
                ));
            }
            let metadata = format::media_metadata_pairs(&item.metadata);
            if !metadata.is_empty() {
                ui.collapsing("Metadata", |ui| {
                    for (key, value) in metadata {
                        ui.label(format!("{key}: {value}"));
                    }
                });
            }
            ui.horizontal(|ui| {
                if ui
                    .button(if item.is_favorite {
                        "Unfavorite"
                    } else {
                        "Favorite"
                    })
                    .clicked()
                {
                    app.send(Command::SetMediaFavorite {
                        id: item.id,
                        favorite: !item.is_favorite,
                    });
                }
                if ui.button("Send to Discord").clicked() {
                    app.send(Command::SendMediaToDiscord {
                        path: item.path.clone(),
                    });
                }
            });
        });
    if !open {
        app.media_selected = None;
    }
}

fn fmt_bytes(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes.max(0) as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// Discord webhooks section
// ---------------------------------------------------------------------------

fn webhooks_section(app: &mut VrcNextApp, ui: &mut egui::Ui, state: &vrc_core::AppState) {
    egui::CollapsingHeader::new(format!("Discord webhooks ({})", state.webhooks.len()))
        .default_open(false)
        .show(ui, |ui| {
            ui.weak("Webhook URLs are stored only in the OS credential store — never in the database or logs.");
            for (index, slot) in state.webhooks.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("Webhook {}", index + 1));
                    let mut autopost = slot.autopost;
                    if ui
                        .checkbox(&mut autopost, "auto-post photos")
                        .changed()
                    {
                        app.send(Command::SetWebhookAutopost {
                            index,
                            enabled: autopost,
                        });
                    }
                    if ui.small_button("Remove").clicked() {
                        app.send(Command::RemoveWebhook { index });
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut app.media_webhook_input)
                        .hint_text("https://discord.com/api/webhooks/…")
                        .desired_width(320.0),
                );
                let mut autopost = app.media_webhook_autopost;
                if ui.checkbox(&mut autopost, "auto-post").changed() {
                    app.media_webhook_autopost = autopost;
                }
                let can_add = !app.media_webhook_input.trim().is_empty()
                    && state.webhooks.len() < vrc_core::MAX_WEBHOOKS;
                if ui
                    .add_enabled(can_add, egui::Button::new("Add webhook"))
                    .clicked()
                {
                    let url = SecretString::from(app.media_webhook_input.trim().to_string());
                    app.media_webhook_input.clear();
                    app.send(Command::AddWebhook { url, autopost });
                }
            });
        });
}
