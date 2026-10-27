//! Окно HexCleaner. Вся логика очистки — в библиотеке `hexcleaner` (`src/lib.rs`).

// В релизной сборке не показываем консольное окно.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod disk_tab;
mod progress;
mod schedule_tab;
mod settings_tab;
mod update_ui;
mod widgets;

use eframe::egui;

use hexcleaner::cli::{self, ExitCode};
use hexcleaner::settings::Settings;
use hexcleaner::{i18n, schedule, system, t};

/// Имя программы в справке.
const EXE_NAME: &str = "HexCleaner";

fn main() -> eframe::Result {
    // С параметрами окно не открывается: выполняем команду или показываем справку.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let exit_code = run_with_arguments(&args);
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

/// Запуск с параметрами. Понимает только `--scheduled <команда>` и справку;
/// на остальное отвечает ошибкой со справкой, а не открывает окно.
fn run_with_arguments(args: &[String]) -> ExitCode {
    let has_console = system::console::attach_parent_console();

    // Плановая очистка из Планировщика заданий: те же команды, что у HexCleanerCli.exe,
    // но без мелькающей консоли (см. hexcleaner::schedule).
    if args[0] == schedule::SCHEDULED_FLAG {
        return cli::run(&args[1..], &format!("{EXE_NAME} {}", schedule::SCHEDULED_FLAG));
    }

    if let Some(error) = i18n::select_language(Settings::load().language.as_deref()) {
        eprintln!("{error}");
    }
    let help = t!("gui.help", exe = EXE_NAME, scheduled = schedule::SCHEDULED_FLAG);
    let is_help = |arg: &String| matches!(arg.as_str(), "-h" | "--help" | "/?");
    let (text, exit_code) = match args {
        [arg] if is_help(arg) => (help, ExitCode::Success),
        _ => {
            let unknown = args.iter().find(|arg| !is_help(arg)).unwrap_or(&args[0]);
            (format!("{}\n\n{help}", t!("gui.error.unknown_argument", value = unknown)), ExitCode::BadArguments)
        }
    };

    let is_error = exit_code != ExitCode::Success;
    if !has_console {
        system::console::message_box(&t!("app.name"), &text, is_error);
    } else if is_error {
        eprintln!("{text}");
    } else {
        println!("{text}");
    }
    exit_code
}
