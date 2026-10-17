// В релизной сборке не показываем консольное окно.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod catalog;
mod cleaner;
mod settings;
mod system;
mod widgets;

use eframe::egui;

fn main() -> eframe::Result {
    let is_admin = system::admin::is_elevated();
    let title = if is_admin { "HexCleaner — Администратор" } else { "HexCleaner" };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([780.0, 560.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "HexCleaner",
        options,
        Box::new(move |cc| Ok(Box::new(app::HexCleanerApp::new(cc, is_admin)))),
    )
}
