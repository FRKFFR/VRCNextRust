//! egui/eframe front end; reads `StateHandle`, sends `Command`s.

use eframe::egui;

/// Root eframe application.
pub struct VrcNextApp {
    pub handle: vrc_core::AppHandle,
}

impl eframe::App for VrcNextApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Frame::central_panel(ui.style()).show(ui, |_ui| {});
    }
}

/// Open the "VRCNextRust" window and run the UI loop.
pub fn run(handle: vrc_core::AppHandle) -> eframe::Result<()> {
    eframe::run_native(
        "VRCNextRust",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(VrcNextApp { handle }))),
    )
}
