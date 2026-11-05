//! `vrcnextrust` binary: init tracing, spawn the core, run the egui UI.

use eframe::egui;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt::init();
    // The credential store is best-effort: login still works in memory when
    // it is unavailable (RULES.md §4: login is optional where possible).
    let builder = vrc_core::Client::builder();
    let client = match vrc_core::KeyringVault::new() {
        Ok(vault) => builder.vault(vault).build(),
        Err(e) => {
            tracing::warn!("credential store unavailable, session not persisted: {e}");
            builder.build()
        }
    }
    .expect("REST client builds");
    let handle = vrc_core::App::spawn(vrc_core::AppConfig::new(client));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VRCNextRust")
            .with_app_id("vrcnextrust")
            .with_inner_size(egui::vec2(1100.0, 720.0))
            .with_min_inner_size(egui::vec2(760.0, 480.0)),
        ..Default::default()
    };
    ui_egui::run(handle, options)
}
