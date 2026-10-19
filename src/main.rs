//! Окно HexCleaner. Вся логика очистки — в библиотеке `hexcleaner` (`src/lib.rs`).

// В релизной сборке не показываем консольное окно.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod progress;
mod schedule_tab;
mod widgets;

use eframe::egui;

use hexcleaner::settings::Settings;
use hexcleaner::{i18n, schedule, system};

fn main() -> eframe::Result {
    // Плановая очистка из Планировщика заданий: та же команда, что у HexCleanerCli.exe,
    // но без окна и без мелькающей консоли (см. hexcleaner::schedule).
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(schedule::SCHEDULED_FLAG) {
        let exit_code = hexcleaner::cli::run(&args[1..]);
        std::process::exit(exit_code as i32);
    }

    let is_admin = system::admin::is_elevated();
    let settings = Settings::load();

    // Встроенные переводы выкладываем в папку языков как образцы для своих.
    i18n::export_built_in_languages();
    let language_error = i18n::select_language(settings.language.as_deref());

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(app::window_title(is_admin))
        .with_inner_size([780.0, 560.0])
        .with_min_inner_size([600.0, 400.0]);
    // Иконка окна и панели задач — та же, что вшита в exe (см. examples/make_icon.rs).
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        "HexCleaner",
        options,
        Box::new(move |cc| Ok(Box::new(app::HexCleanerApp::new(cc, settings, is_admin, language_error)))),
    )
}
