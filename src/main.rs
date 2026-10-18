// В релизной сборке не показываем консольное окно.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod catalog;
mod cleaner;
mod i18n;
mod presets;
mod settings;
mod system;
mod widgets;

use eframe::egui;

use settings::Settings;

fn main() -> eframe::Result {
    let is_admin = system::admin::is_elevated();
    let settings = Settings::load();

    // Встроенные переводы выкладываем в папку языков как образцы для своих.
    i18n::export_built_in_languages();
    let language_error = select_startup_language(&settings);

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

/// Включает сохранённый язык или, если он не выбран, язык Windows.
/// Если файл сохранённого языка пропал или испорчен, включает язык Windows
/// и возвращает текст ошибки для строки состояния.
fn select_startup_language(settings: &Settings) -> Option<String> {
    let code = settings.language.clone().unwrap_or_else(|| i18n::system_language().to_owned());
    match i18n::set_language(&code) {
        Ok(()) => None,
        Err(error) => {
            let _ = i18n::set_language(i18n::system_language());
            Some(t!("status.language_failed", code = code, error = error))
        }
    }
}
