//! Окно HexCleaner. Вся логика очистки — в библиотеке `hexcleaner` (`src/lib.rs`).

// В релизной сборке не показываем консольное окно.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod widgets;

use eframe::egui;

use hexcleaner::settings::Settings;
use hexcleaner::{i18n, system};

fn main() -> eframe::Result {
    let is_admin = system::admin::is_elevated();
    let settings = Settings::load();

    // Встроенные переводы выкладываем в папку языков как образцы для своих.
    i18n::export_built_in_languages();
    let language_error = i18n::select_language(settings.language.as_deref());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app::window_title(is_admin))
            .with_inner_size([780.0, 560.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "HexCleaner",
        options,
        Box::new(move |cc| Ok(Box::new(app::HexCleanerApp::new(cc, settings, is_admin, language_error)))),
    )
}
