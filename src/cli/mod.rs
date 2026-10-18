//! Командная строка: `HexCleanerCli analyze | clean | list | help`.
//!
//! Работает на том же движке, что и окно, и с теми же настройками: отмеченными пунктами,
//! своими наборами, языком и сжатием журналов. Настройки только читаются.
//! Справка с примерами — `HexCleanerCli help` (текст в файлах перевода, ключ `cli.help`).

mod args;
mod selection;

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use crate::catalog::{self, CleanupTask, Safety};
use crate::cleaner::{
    BlockingApp, CleanupEvent, CleanupJob, CleanupOptions, Mode, TaskReport, TaskResult, find_blocking_apps, format_size,
};
use crate::presets;
use crate::settings::Settings;
use crate::system::{admin, console};
use crate::{i18n, t};

use args::{Command, RunRequest};

/// Имя программы в справке и сообщениях об ошибках.
const EXE_NAME: &str = "HexCleanerCli";

/// Как часто проверять, не нажат ли Ctrl+C, пока идёт очистка.
const CTRL_C_CHECK_INTERVAL: Duration = Duration::from_millis(200);

/// Коды возврата. Их показывает планировщик Windows в «Результате последнего запуска».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCode {
    Success = 0,
    /// Ошибка в параметрах: неизвестная команда, набор или пункт.
    BadArguments = 1,
    /// Готово, но часть файлов не удалось удалить или часть пунктов пропущена без прав администратора.
    PartiallyDone = 2,
    /// Не удалось начать, например создать журнал.
    Failed = 3,
    /// Остановлено по Ctrl+C.
    Cancelled = 4,
}

/// Ошибка в параметрах командной строки.
#[derive(Debug, PartialEq)]
pub enum UsageError {
    UnknownCommand(String),
    UnknownOption(String),
    MissingValue(String),
    /// Параметр анализа или очистки указан у `list` или `help`.
    OptionNotForCommand(String),
    UnknownPreset(String),
    UnknownTask(String),
    NothingSelected,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self {
            UsageError::UnknownCommand(value) => t!("cli.error.unknown_command", value = value, exe = EXE_NAME),
            UsageError::UnknownOption(value) => t!("cli.error.unknown_option", value = value, exe = EXE_NAME),
            UsageError::MissingValue(option) => t!("cli.error.missing_value", option = option),
            UsageError::OptionNotForCommand(option) => t!("cli.error.option_not_for_command", option = option),
            UsageError::UnknownPreset(name) => t!("cli.error.unknown_preset", name = name, exe = EXE_NAME),
            UsageError::UnknownTask(id) => t!("cli.error.unknown_task", id = id, exe = EXE_NAME),
            UsageError::NothingSelected => t!("cli.error.nothing_selected"),
        };
        f.write_str(&text)
    }
}

/// Выполняет команду (`args` — параметры без имени программы) и возвращает код возврата.
pub fn run(args: &[String]) -> ExitCode {
    let settings = Settings::load();
    if let Some(error) = i18n::select_language(settings.language.as_deref()) {
        eprintln!("{error}");
    }

    let arguments = match args::parse(args) {
        Ok(arguments) => arguments,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::BadArguments;
        }
    };
    if let Some(code) = &arguments.language
        && let Some(error) = i18n::select_language(Some(code))
    {
        eprintln!("{error}");
    }

    match arguments.command {
        Command::Help => {
            println!("{}", t!("cli.help", exe = EXE_NAME));
            ExitCode::Success
        }
        Command::List => {
            print_list(&settings);
            ExitCode::Success
        }
        Command::Run(request) => run_cleanup(&request, &settings),
    }
}

/// Пункты с идентификаторами и наборы — всё, что можно указать в `--tasks` и `--preset`.
fn print_list(settings: &Settings) {
    let tasks = catalog::all_tasks();
    let id_width = tasks.iter().map(|task| task.id.len()).max().unwrap_or(0);
    let level_width = Safety::ALL.iter().map(|level| level.name().chars().count()).max().unwrap_or(0);

    println!("{}", t!("cli.list.tasks"));
    for task in &tasks {
        let mark = if settings.is_task_selected(task) { "[x]" } else { "[ ]" };
        let admin = if task.requires_admin { t!("cli.list.admin") } else { String::new() };
        let level = task.safety.name();
        println!("  {mark} {:<id_width$}  {level:<level_width$}  {}{admin}", task.id, task.title());
    }
    println!();
    println!("{}", t!("cli.list.selected_hint"));

    println!();
    println!("{}", t!("cli.list.presets"));
    for level in Safety::ALL {
        println!("  {:<id_width$}  {}", presets::built_in_id(level), presets::built_in_name(level));
    }
    for preset in &settings.presets {
        let name = format!("\"{}\"", preset.name);
        println!("  {name:<id_width$}  {}", t!("cli.list.user_preset", count = preset.tasks.len()));
    }
}

/// Анализ или очистка с выводом хода работы.
fn run_cleanup(request: &RunRequest, settings: &Settings) -> ExitCode {
    let tasks = Arc::new(catalog::all_tasks());
    let selected = match selection::resolve(&request.choice, &tasks, settings) {
        Ok(selected) => selected,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::BadArguments;
        }
    };

    let is_admin = admin::is_elevated();
    if !request.quiet {
        println!("{}", header(request.mode));
        if !is_admin && selected.iter().any(|&index| tasks[index].requires_admin) {
            println!("{}", t!("cli.admin_warning"));
        }
    }

    // Программы, мешающие очистке, ищем только по запросу: это занимает время.
    let blocking_apps = if request.close_apps { find_blocking_apps(&tasks, &selected) } else { Vec::new() };
    if !request.quiet {
        print_blocking_apps(&blocking_apps, request.mode);
        println!();
    }

    let options = CleanupOptions {
        mode: request.mode,
        is_admin,
        compress_old_logs: request.compress_logs.unwrap_or(settings.compress_old_logs),
        close_programs: blocking_apps.iter().flat_map(|app| app.exes.clone()).collect(),
        restart_programs: request.restart_apps,
    };

    console::catch_ctrl_c();
    let mut progress = Progress::new(Arc::clone(&tasks), selected.len(), request);
    let job = CleanupJob::start(tasks, selected, options);
    let mut stopping = false;

    loop {
        match job.wait_event(CTRL_C_CHECK_INTERVAL) {
            Ok(CleanupEvent::ClosingPrograms) => {}
            Ok(CleanupEvent::TaskStarted(index)) => progress.task_started(index),
            Ok(CleanupEvent::Progress(..)) => {}
            Ok(CleanupEvent::TaskFinished(index, result)) => progress.task_finished(index, &result),
            Ok(CleanupEvent::TaskSkipped(index)) => progress.task_skipped(index),
            Ok(CleanupEvent::Finished { total, log_path, cancelled }) => {
                return progress.finish(&total, &log_path, cancelled);
            }
            Ok(CleanupEvent::Failed(message)) => {
                eprintln!("{message}");
                return ExitCode::Failed;
            }
            Err(RecvTimeoutError::Timeout) => {
                if console::ctrl_c_pressed() && !stopping {
                    stopping = true;
                    eprintln!("\n{}", t!("cli.stopping"));
                    job.cancel();
                }
            }
            Err(RecvTimeoutError::Disconnected) => return ExitCode::Failed,
        }
    }
}

/// «Закрываю программы: Google Chrome (chrome.exe) — мешает: …». При анализе — только список.
fn print_blocking_apps(apps: &[BlockingApp], mode: Mode) {
    if apps.is_empty() {
        return;
    }
    println!(
        "{}",
        match mode {
            Mode::Clean => t!("cli.closing_apps"),
            Mode::Analyze => t!("cli.blocking_apps"),
        }
    );
    let tasks = catalog::all_tasks();
    for app in apps {
        let blocks: Vec<String> = app.tasks.iter().map(|&index| tasks[index].title()).collect();
        println!("  {} — {}", app.label(), t!("apps.blocks", tasks = blocks.join("; ")));
    }
}

fn header(mode: Mode) -> String {
    match mode {
        Mode::Analyze => t!("cli.header_analysis"),
        Mode::Clean => t!("cli.header_clean"),
    }
}

/// Вывод хода работы: «[2/5] Название пункта … файлов: 12 · 3.4 МБ».
struct Progress {
    tasks: Arc<Vec<CleanupTask>>,
    mode: Mode,
    /// Сколько пунктов выбрано и сколько из них уже начато.
    count: usize,
    started: usize,
    skipped: usize,
    details: bool,
    quiet: bool,
}

impl Progress {
    fn new(tasks: Arc<Vec<CleanupTask>>, count: usize, request: &RunRequest) -> Self {
        Self { tasks, mode: request.mode, count, started: 0, skipped: 0, details: request.details, quiet: request.quiet }
    }

    /// Начало строки пункта; результат допишется в ту же строку, когда пункт закончится.
    fn task_started(&mut self, index: usize) {
        self.started += 1;
        if !self.quiet {
            print!("[{}/{}] {} … ", self.started, self.count, self.tasks[index].title());
            let _ = std::io::stdout().flush();
        }
    }

    fn task_finished(&mut self, _index: usize, result: &TaskResult) {
        if self.quiet {
            return;
        }
        println!("{}", result.note.clone().unwrap_or_else(|| result.report.summary()));
        if self.details {
            for location in &result.locations {
                let files = t!("locations.files", count = location.files);
                println!("      {:>10}  {files:<14}  {}", format_size(location.bytes), location.path.display());
            }
        }
    }

    /// Пункт пропущен без прав администратора — для него не было `task_started`.
    fn task_skipped(&mut self, index: usize) {
        self.skipped += 1;
        self.task_started(index);
        if !self.quiet {
            println!("{}", t!("list.skipped"));
        }
    }

    fn finish(&self, total: &TaskReport, log_path: &Path, cancelled: bool) -> ExitCode {
        if !self.quiet {
            println!();
        }
        if cancelled {
            println!("{}", t!("cli.cancelled"));
        }
        let size = format_size(total.freed_bytes);
        match self.mode {
            Mode::Analyze => println!("{}", t!("cli.total_analysis", files = total.deleted_files, size = size)),
            Mode::Clean => println!(
                "{}",
                t!("cli.total_clean", files = total.deleted_files, size = size, failed = total.failed)
            ),
        }
        if total.freed_memory > 0 {
            println!("{}", t!("cli.total_memory", size = format_size(total.freed_memory)));
        }
        println!("{}", t!("cli.log", path = log_path.display()));

        if cancelled {
            ExitCode::Cancelled
        } else if total.failed > 0 || self.skipped > 0 {
            ExitCode::PartiallyDone
        } else {
            ExitCode::Success
        }
    }
}
