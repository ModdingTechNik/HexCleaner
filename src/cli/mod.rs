//! Командная строка: `HexCleanerCli analyze | clean | list | config | trash | help`.
//! Те же команды выполняет и окно с ключом `--scheduled` (см. `hexcleaner::schedule`).
//!
//! Работает на том же движке, что и окно, и с теми же настройками: отмеченными пунктами,
//! своими наборами, языком, сжатием журналов и корзиной. Меняет настройки только `config`.
//! Справка с примерами — `HexCleanerCli help` (текст в файлах перевода, ключ `cli.help`).

mod args;
mod config;
mod selection;
mod trash;

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use crate::catalog::{self, CleanupTask, Safety};
use crate::custom_tasks;
use crate::cleaner::{
    BlockingApp, CleanupEvent, CleanupJob, CleanupOptions, Mode, TaskReport, TaskResult, find_blocking_apps, format_size,
};
use crate::presets;
use crate::settings::Settings;
use crate::system::{admin, console};
use crate::{i18n, t};

use args::{Command, RunRequest};

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
    /// Параметр не подходит к команде: например, `--quiet` у `list`.
    OptionNotForCommand { option: String, command: String },
    UnknownPreset(String),
    NothingSelected,
    /// Нет такой настройки (`config`).
    UnknownSetting(String),
    /// Настройке нельзя задать такое значение; `expected` — какие можно.
    InvalidSetting { key: String, value: String, expected: String },
    /// В корзине нет такой очистки (`trash`).
    UnknownTrashRun(String),
    /// В очистке `run` нет пункта `id`; `ids` — какие есть.
    UnknownTrashItem { id: String, run: String, ids: String },
    /// В очистке нет файлов, подходящих под `--tasks` и `--path`.
    NothingToRestore(String),
    /// Удаление из корзины навсегда без `--yes`; `command` — как повторить.
    ConfirmationRequired(String),
}

impl UsageError {
    /// Текст ошибки; `exe` — как запускать команды, для подсказок вида «Справка: {exe} help».
    fn message(&self, exe: &str) -> String {
        match self {
            UsageError::UnknownCommand(value) => t!("cli.error.unknown_command", value = value, exe = exe),
            UsageError::UnknownOption(value) => t!("cli.error.unknown_option", value = value, exe = exe),
            UsageError::MissingValue(option) => t!("cli.error.missing_value", option = option),
            UsageError::OptionNotForCommand { option, command } => {
                t!("cli.error.option_not_for_command", option = option, command = command, exe = exe)
            }
            UsageError::UnknownPreset(name) => t!("cli.error.unknown_preset", name = name, exe = exe),
            UsageError::NothingSelected => t!("cli.error.nothing_selected"),
            UsageError::UnknownSetting(key) => t!("cli.error.unknown_setting", key = key, exe = exe),
            UsageError::InvalidSetting { key, value, expected } => {
                t!("cli.error.invalid_setting", key = key, value = value, expected = expected)
            }
            UsageError::UnknownTrashRun(name) => t!("cli.error.unknown_trash_run", name = name, exe = exe),
            UsageError::UnknownTrashItem { id, run, ids } => t!("cli.error.unknown_trash_item", id = id, run = run, ids = ids),
            UsageError::NothingToRestore(run) => t!("cli.error.nothing_to_restore", run = run),
            UsageError::ConfirmationRequired(command) => {
                t!("cli.error.confirmation_required", command = command, exe = exe)
            }
        }
    }
}

/// Выполняет команду (`args` — параметры без имени программы) и возвращает код возврата.
/// `exe` — как запускать команды: так программа называется в справке и сообщениях об ошибках,
/// например `HexCleanerCli` или `HexCleaner --scheduled`.
pub fn run(args: &[String], exe: &str) -> ExitCode {
    let mut settings = Settings::load();
    if let Some(error) = i18n::select_language(settings.language.as_deref()) {
        eprintln!("{error}");
    }

    let arguments = match args::parse(args) {
        Ok(arguments) => arguments,
        Err(error) => {
            eprintln!("{}", error.message(exe));
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
            println!("{}", t!("cli.help", exe = exe));
            ExitCode::Success
        }
        Command::List => {
            print_list(&settings);
            ExitCode::Success
        }
        Command::Config(command) => config::run(&command, &mut settings, exe),
        Command::Trash(command) => trash::run(&command, &settings, exe),
        Command::Run(request) => run_cleanup(&request, &settings, exe),
    }
}

/// Пункты с идентификаторами и наборы — всё, что можно указать в `--tasks` и `--preset`.
fn print_list(settings: &Settings) {
    let (tasks, errors) = catalog::load_tasks();
    let id_width = tasks.iter().map(|task| task.id.len()).max().unwrap_or(0);
    let level_width = Safety::ALL.iter().map(|level| level.name().chars().count()).max().unwrap_or(0);

    println!("{}", t!("cli.list.tasks"));
    for task in &tasks {
        let mark = if settings.is_task_selected(task) { "[x]" } else { "[ ]" };
        let admin = if task.requires_admin { t!("cli.list.admin") } else { String::new() };
        let custom = task.custom.as_ref().map(|custom| t!("cli.list.custom", file = custom.file)).unwrap_or_default();
        let level = task.safety.name();
        println!("  {mark} {:<id_width$}  {level:<level_width$}  {}{admin}{custom}", task.id, task.title());
    }
    println!();
    println!("{}", t!("cli.list.selected_hint"));
    println!("{}", t!("cli.list.custom_hint", folder = custom_tasks::tasks_directory().display()));
    for error in &errors {
        eprintln!("{error}");
    }

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
fn run_cleanup(request: &RunRequest, settings: &Settings, exe: &str) -> ExitCode {
    let tasks = Arc::new(catalog::all_tasks());
    let resolved = match selection::resolve(&request.choice, &tasks, settings) {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("{}", error.message(exe));
            return ExitCode::BadArguments;
        }
    };
    // Пропущенные пункты видны и при --quiet: в задании планировщика это единственный вывод.
    for id in &resolved.missing {
        eprintln!("{}", t!("cli.warning.unknown_task", id = id, exe = exe));
    }
    let selected = resolved.indices;
    if selected.is_empty() {
        eprintln!("{}", UsageError::NothingSelected.message(exe));
        return ExitCode::BadArguments;
    }

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

    // `--no-trash` — как Shift в окне: корзина отключается, но её папка по-прежнему не очищается.
    let mut trash = settings.trash.clone();
    if request.no_trash {
        trash.limit = Some(0);
    }
    let trash_dir = trash.directory();
    let options = CleanupOptions {
        mode: request.mode,
        is_admin,
        log_compression: request.compress_logs.unwrap_or(settings.compress_old_logs).then_some(settings.log_compression),
        close_programs: blocking_apps.iter().flat_map(|app| app.exes.clone()).collect(),
        restart_programs: request.restart_apps,
        trash: Some(trash),
        // Спросить про лимит корзины некого: не поместившееся удаляется мимо неё.
        ask_trash_limit: false,
        notes: resolved.missing.iter().map(|id| t!("log.unknown_task", id = id)).collect(),
    };

    console::catch_ctrl_c();
    let mut progress = Progress::new(Arc::clone(&tasks), selected.len(), request);
    let job = CleanupJob::start(tasks, selected, options);
    let mut stopping = false;

    loop {
        match job.wait_event(CTRL_C_CHECK_INTERVAL) {
            Ok(CleanupEvent::ClosingPrograms | CleanupEvent::PackingTrash | CleanupEvent::TrashLimit(_)) => {}
            Ok(CleanupEvent::TaskStarted(index)) => progress.task_started(index),
            Ok(CleanupEvent::Progress(..)) => {}
            Ok(CleanupEvent::TaskFinished(index, result)) => progress.task_finished(index, &result),
            Ok(CleanupEvent::TaskSkipped(index)) => progress.task_skipped(index),
            Ok(CleanupEvent::Finished { total, log_path, cancelled, trash_run }) => {
                // Со сжатием ZIP папка очистки уже упакована в `<очистка>.zip`.
                let trash_run = trash_run.map(|id| {
                    let archive = trash_dir.join(format!("{id}.zip"));
                    if archive.is_file() { archive } else { trash_dir.join(id) }
                });
                let code = progress.finish(&total, &log_path, trash_run.as_deref(), cancelled, exe);
                // Пропущенные пункты — повод заглянуть в журнал, как и неудалённые файлы.
                return match code {
                    ExitCode::Success if !resolved.missing.is_empty() => ExitCode::PartiallyDone,
                    code => code,
                };
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

    /// `trash_run` — папка этой очистки в корзине, если в неё что-то перенесено.
    fn finish(&self, total: &TaskReport, log_path: &Path, trash_run: Option<&Path>, cancelled: bool, exe: &str) -> ExitCode {
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
        if let Some(run) = trash_run
            && total.trashed_files > 0
        {
            let size = format_size(total.trashed_bytes);
            let path = run.display();
            println!("{}", t!("cli.total_trash", files = total.trashed_files, size = size, path = path, exe = exe));
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
