//! Команда `trash`: корзина HexCleaner — то же, что вкладка «Корзина» окна.
//!
//! * `trash` или `trash list` — очистки в корзине, новые первыми, с номерами;
//! * `trash show <очистка> [--files]` — пункты очистки (и их файлы);
//! * `trash restore <очистка> [--tasks <id,…>] [--path <папка или файл>]` — вернуть файлы на место;
//! * `trash delete <очистка> --yes`, `trash empty --yes` — удалить навсегда.
//!
//! Очистку можно указать номером из списка (1 — самая новая), словом `latest` или именем папки
//! в корзине, например `"2026-10-29 10-56-42"`.

use std::path::{Path, PathBuf};

use chrono::Local;

use crate::cleaner::format_size;
use crate::settings::Settings;
use crate::t;
use crate::trash::{Trash, TrashFile, TrashItem, TrashRun};

use super::{ExitCode, UsageError};

/// Что сделать с корзиной.
#[derive(Debug, PartialEq)]
pub enum TrashCommand {
    List,
    Show { run: String, files: bool },
    /// Пустые `tasks` и `paths` — восстановить всю очистку.
    Restore { run: String, tasks: Vec<String>, paths: Vec<PathBuf> },
    /// `confirmed` — указан `--yes`: удаление навсегда без него не выполняется.
    Delete { run: String, confirmed: bool },
    Empty { confirmed: bool },
}

/// Выполняет команду `trash`; `exe` — как запускать команды (для подсказок в ошибках).
pub fn run(command: &TrashCommand, settings: &Settings, exe: &str) -> ExitCode {
    let trash = Trash::new(settings.trash.directory());
    // Как при запуске окна: очистки старше срока хранения удаляются.
    if let Some(days) = settings.trash.keep_days {
        trash.remove_expired(days, Local::now());
    }

    let result = match command {
        TrashCommand::List => {
            list(&trash, settings, exe);
            Ok(ExitCode::Success)
        }
        TrashCommand::Show { run, files } => find_run(&trash, run).map(|run| {
            show(&run, *files);
            ExitCode::Success
        }),
        TrashCommand::Restore { run, tasks, paths } => {
            find_run(&trash, run).and_then(|run| restore(&trash, &run, tasks, paths))
        }
        TrashCommand::Delete { run, confirmed } => find_run(&trash, run).and_then(|run| {
            confirm(*confirmed, &format!("trash delete \"{}\"", run.id))?;
            Ok(deleted(trash.delete_run(&run.id)))
        }),
        TrashCommand::Empty { confirmed } => {
            confirm(*confirmed, "trash empty").map(|()| deleted(trash.empty()))
        }
    };
    result.unwrap_or_else(|error| {
        eprintln!("{}", error.message(exe));
        ExitCode::BadArguments
    })
}

/// Очистки в корзине: номер, имя, размер и число файлов.
fn list(trash: &Trash, settings: &Settings, exe: &str) {
    let runs = trash.runs();
    let limit = match settings.trash.limit {
        Some(0) => t!("trash.limit_disabled"),
        Some(limit) => format_size(limit),
        None => t!("trash.limit_unlimited"),
    };
    println!("{}", t!("cli.trash.folder", path = trash.dir().display()));
    println!("{}", t!("trash.used", used = format_size(runs.iter().map(|run| run.stored_bytes).sum()), limit = limit));
    println!();
    if runs.is_empty() {
        println!("{}", t!("trash.nothing"));
        return;
    }
    let number_width = runs.len().to_string().len();
    let id_width = runs.iter().map(|run| run.id.chars().count()).max().unwrap_or(0);
    for (index, run) in runs.iter().enumerate() {
        println!("  {:>number_width$}  {:<id_width$}  {}", index + 1, run.id, run_summary(run));
    }
    println!();
    println!("{}", t!("cli.trash.list_hint", exe = exe));
}

/// «6 Б, файлов: 2 (zip, 3 Б)».
fn run_summary(run: &TrashRun) -> String {
    let mut text = t!("cli.trash.run", size = format_size(run.total_size()), files = run.file_count());
    if run.archived {
        text += &t!("trash.run_zip", size = format_size(run.stored_bytes));
    }
    text
}

/// Пункты очистки с идентификаторами для `--tasks`; с `files` — и файлы.
fn show(run: &TrashRun, files: bool) {
    println!("{}  {}", run.id, run_summary(run));
    let id_width = run.items.iter().map(|item| item_id(item).chars().count()).max().unwrap_or(0);
    for item in &run.items {
        let summary = t!("trash.item", title = item.title, size = format_size(item.total_size()), files = item.files.len());
        println!("  {:<id_width$}  {summary}", item_id(item));
        if files {
            for file in &item.files {
                println!("      {:>10}  {}", format_size(file.size), file.path.display());
            }
        }
    }
}

/// Идентификатор пункта для `--tasks`; у файлов без описания его нет — показываем «-».
fn item_id(item: &TrashItem) -> &str {
    if item.task.is_empty() { "-" } else { &item.task }
}

fn restore(trash: &Trash, run: &TrashRun, tasks: &[String], paths: &[PathBuf]) -> Result<ExitCode, UsageError> {
    for task in tasks {
        if !run.items.iter().any(|item| item_id(item).eq_ignore_ascii_case(task)) {
            let ids: Vec<&str> = run.items.iter().map(item_id).collect();
            return Err(UsageError::UnknownTrashItem { id: task.clone(), run: run.id.clone(), ids: ids.join(", ") });
        }
    }
    // Относительные пути — от текущей папки, как их понимает пользователь.
    let paths: Vec<PathBuf> = paths.iter().map(|path| std::path::absolute(path).unwrap_or_else(|_| path.clone())).collect();
    let filter = |item: &TrashItem, file: &TrashFile| {
        (tasks.is_empty() || tasks.iter().any(|task| item_id(item).eq_ignore_ascii_case(task)))
            && (paths.is_empty() || paths.iter().any(|path| is_within(&file.path, path)))
    };
    if !run.items.iter().any(|item| item.files.iter().any(|file| filter(item, file))) {
        return Err(UsageError::NothingToRestore(run.id.clone()));
    }

    let report = trash.restore(&run.id, filter);
    println!("{}", report.summary());
    for (path, error) in &report.failed {
        eprintln!("  {}: {error}", path.display());
    }
    Ok(if report.failed.is_empty() && report.existing == 0 { ExitCode::Success } else { ExitCode::PartiallyDone })
}

/// `file` — это `path` или лежит внутри папки `path` (без учёта регистра, как в Windows).
fn is_within(file: &Path, path: &Path) -> bool {
    let (file, path) = (file.to_string_lossy().to_lowercase(), path.to_string_lossy().to_lowercase());
    let path = path.trim_end_matches('\\');
    file == path || file.starts_with(&format!("{path}\\"))
}

/// Очистка по номеру из списка, слову `latest` или имени.
fn find_run(trash: &Trash, name: &str) -> Result<TrashRun, UsageError> {
    let mut runs = trash.runs();
    let index = match name.trim() {
        name if name.eq_ignore_ascii_case("latest") => Some(0),
        name => match name.parse::<usize>() {
            Ok(number) => number.checked_sub(1),
            Err(_) => runs.iter().position(|run| run.id.eq_ignore_ascii_case(name)),
        },
    };
    index
        .filter(|&index| index < runs.len())
        .map(|index| runs.swap_remove(index))
        .ok_or_else(|| UsageError::UnknownTrashRun(name.to_owned()))
}

/// Удаление навсегда — только с `--yes`.
fn confirm(confirmed: bool, command: &str) -> Result<(), UsageError> {
    if confirmed { Ok(()) } else { Err(UsageError::ConfirmationRequired(command.to_owned())) }
}

fn deleted(result: std::io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => {
            println!("{}", t!("trash.deleted"));
            ExitCode::Success
        }
        Err(error) => {
            eprintln!("{}", t!("trash.error.delete", error = error));
            ExitCode::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_files_inside_folder_case_insensitively() {
        let file = Path::new(r"C:\Users\Me\AppData\Local\Temp\sub\a.tmp");
        assert!(is_within(file, Path::new(r"c:\users\me\appdata\local\temp")));
        assert!(is_within(file, Path::new(r"C:\Users\Me\AppData\Local\Temp\")));
        assert!(is_within(file, file));
        // Папка с тем же началом имени — другая папка.
        assert!(!is_within(file, Path::new(r"C:\Users\Me\AppData\Local\Te")));
        assert!(!is_within(file, Path::new(r"C:\Users\Me\AppData\Local\Temp\sub\a.tmp.bak")));
    }

    #[test]
    fn finds_runs_by_number_latest_and_name() {
        let dir = std::env::temp_dir().join(format!("hexcleaner_cli_trash_{}", std::process::id()));
        let trash = Trash::new(dir.clone());
        for id in ["2026-10-01 10-00-00", "2026-10-02 10-00-00"] {
            let file = dir.join(id).join(r"c\junk.tmp");
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "junk").unwrap();
        }

        assert_eq!(find_run(&trash, "latest").unwrap().id, "2026-10-02 10-00-00");
        assert_eq!(find_run(&trash, "1").unwrap().id, "2026-10-02 10-00-00");
        assert_eq!(find_run(&trash, "2").unwrap().id, "2026-10-01 10-00-00");
        assert_eq!(find_run(&trash, "2026-10-01 10-00-00").unwrap().id, "2026-10-01 10-00-00");
        for name in ["0", "3", "2026-10-03 10-00-00"] {
            assert_eq!(find_run(&trash, name).unwrap_err(), UsageError::UnknownTrashRun(name.into()));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
