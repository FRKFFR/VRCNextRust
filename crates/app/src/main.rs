//! `vrcnextrust` binary: init tracing, spawn the core, run the egui UI.

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt::init();
    let handle = vrc_core::App::spawn(vrc_core::AppConfig);
    ui_egui::run(handle)
}
