//! Программы, которые мешают очистке: держат открытыми файлы выбранных пунктов
//! (например, запущенный браузер держит свой кэш). Перед очисткой их можно закрыть.
//!
//! Предлагаются только программы из `CleanupTask::programs_to_close` (браузеры, лаунчеры):
//! общие папки держат и посторонние программы, и компоненты Windows — их не трогаем.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use super::{files, paths};
use crate::catalog::{Action, CleanupTask, Program};
use crate::system::restart_manager::{AffectedApp, AppKind, Session};

/// Сколько файлов брать для проверки из одной папки. Программы держат открытыми прежде
/// всего индексы и служебные файлы кэша, а они лежат на верхних уровнях папки, —
/// поэтому папка обходится по уровням, а не вглубь, и полностью её читать не нужно.
const SAMPLE_FILES_PER_FOLDER: usize = 300;

/// Программа, которая мешает очистке.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockingApp {
    /// Главный исполняемый файл, например `chrome.exe`.
    pub exe: String,
    /// Все файлы программы, которые нужно закрыть: главный и вспомогательные.
    /// У браузера много процессов, и закрывать нужно программу целиком.
    pub exes: Vec<String>,
    /// Название программы, например «Google Chrome».
    pub name: String,
    /// Индексы пунктов (в каталоге), которым она мешает.
    pub tasks: Vec<usize>,
}

impl BlockingApp {
    /// «Google Chrome (chrome.exe)» или просто «tool.exe», если отдельного названия нет.
    pub fn label(&self) -> String {
        if self.name.eq_ignore_ascii_case(&self.exe) { self.exe.clone() } else { format!("{} ({})", self.name, self.exe) }
    }
}

/// Программы, которые держат файлы выбранных пунктов (`selected` — индексы в `tasks`).
pub fn find_blocking_apps(tasks: &[CleanupTask], selected: &[usize]) -> Vec<BlockingApp> {
    let mut apps: Vec<BlockingApp> = Vec::new();

    for &index in selected.iter().filter(|&&index| !tasks[index].programs_to_close.is_empty()) {
        let task = &tasks[index];
        for app in apps_holding(&sample_files(task)).into_iter().filter(can_be_closed) {
            let Some(program) = program_of(&app, task.programs_to_close) else { continue };
            let is_main_process = app.exe.eq_ignore_ascii_case(program.exe);

            match apps.iter_mut().find(|known| known.exe.eq_ignore_ascii_case(program.exe)) {
                Some(known) => {
                    if !known.tasks.contains(&index) {
                        known.tasks.push(index);
                    }
                    // Название берём у главного процесса: у вспомогательных оно своё («... WebHelper»).
                    if is_main_process && app.kind == AppKind::Window && !app.name.is_empty() {
                        known.name = app.name;
                    }
                }
                None => apps.push(BlockingApp {
                    exe: program.exe.to_owned(),
                    exes: program.all_exes().map(str::to_owned).collect(),
                    name: if is_main_process && !app.name.is_empty() { app.name } else { program.exe.to_owned() },
                    tasks: vec![index],
                }),
            }
        }
    }

    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

/// К какой из разрешённых программ относится процесс (главный или вспомогательный).
fn program_of<'a>(app: &AffectedApp, programs: &'a [Program]) -> Option<&'a Program> {
    programs.iter().find(|program| program.all_exes().any(|exe| exe.eq_ignore_ascii_case(&app.exe)))
}

/// Закрывать предлагаем только обычные программы пользователя. Службы, системные процессы,
/// Проводник и сам HexCleaner не трогаем.
fn can_be_closed(app: &AffectedApp) -> bool {
    let is_user_program = matches!(app.kind, AppKind::Window | AppKind::Console | AppKind::Unknown);
    let current_exe = std::env::current_exe().ok().and_then(|path| Some(path.file_name()?.to_string_lossy().into_owned()));
    let is_us = app.process.pid == std::process::id()
        || current_exe.is_some_and(|exe| exe.eq_ignore_ascii_case(&app.exe));
    is_user_program && app.service.is_none() && !is_us
}

fn apps_holding(files: &[PathBuf]) -> Vec<AffectedApp> {
    if files.is_empty() {
        return Vec::new();
    }
    let apps = Session::start().and_then(|session| {
        session.register_files(files)?;
        session.affected_apps()
    });
    apps.unwrap_or_default()
}

/// Файлы пункта, по которым проверяем, не держит ли их кто-то.
fn sample_files(task: &CleanupTask) -> Vec<PathBuf> {
    let mut sample = Vec::new();
    for action in &task.actions {
        match action {
            Action::ClearDirectory { pattern, .. } | Action::DeleteDirectory { pattern } => {
                for dir in paths::expand(pattern) {
                    let is_real_dir = dir.symlink_metadata().is_ok_and(|meta| meta.is_dir() && !files::is_link(&meta));
                    if is_real_dir && !paths::is_protected(&dir) {
                        sample.extend(sample_folder(&dir, SAMPLE_FILES_PER_FOLDER));
                    }
                }
            }
            Action::DeleteFiles { pattern } => sample.extend(paths::expand(pattern).into_iter().filter(|path| path.is_file())),
            Action::ClearEventLogs | Action::TrimProcessMemory | Action::EmptyRecycleBin => {}
        }
    }
    sample
}

/// До `limit` файлов папки — по уровням: сначала файлы самой папки, затем вложенных.
fn sample_folder(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut sample = Vec::new();
    let mut folders = VecDeque::from([dir.to_owned()]);

    while let Some(folder) = folders.pop_front() {
        let Ok(entries) = fs::read_dir(&folder) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let Ok(metadata) = entry.metadata() else { continue };
            if files::is_link(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                folders.push_back(entry.path());
            } else {
                sample.push(entry.path());
                if sample.len() >= limit {
                    return sample;
                }
            }
        }
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn samples_top_levels_first() {
        let root = std::env::temp_dir().join(format!("hexcleaner_sample_{}", std::process::id()));
        fs::create_dir_all(root.join(r"deep\deeper")).unwrap();
        fs::write(root.join("index"), "").unwrap();
        fs::write(root.join(r"deep\a"), "").unwrap();
        fs::write(root.join(r"deep\deeper\b"), "").unwrap();

        let sample = sample_folder(&root, 2);
        assert_eq!(sample, vec![root.join("index"), root.join(r"deep\a")]);

        let _ = fs::remove_dir_all(&root);
    }

    /// Файл пункта держит другая программа — её и находим. Сам HexCleaner (тест) в список не попадает.
    #[test]
    fn finds_program_holding_task_files_but_not_itself() {
        let root = std::env::temp_dir().join(format!("hexcleaner_blockers_{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let held = root.join("held.tmp");
        fs::write(&held, "").unwrap();
        let task = CleanupTask {
            id: "test",
            safety: crate::catalog::Safety::Safe,
            programs_to_close: &[Program { exe: "powershell.exe", helpers: &[] }],
            requires_admin: false,
            services_to_stop: &[],
            actions: vec![Action::ClearDirectory { pattern: root.display().to_string(), min_age: None }],
        };

        // Сначала файл держит только сам тест — такую программу предлагать закрыть нельзя.
        let file = OpenOptions::new().read(true).share_mode(1).open(&held).unwrap();
        assert!(find_blocking_apps(std::slice::from_ref(&task), &[0]).is_empty());
        drop(file);

        // Теперь файл держит отдельная программа (PowerShell, ждущий 20 секунд).
        let script = format!(
            "$f = [IO.File]::Open('{}', 'Open', 'Read', 'Read'); Start-Sleep 20",
            held.display()
        );
        let mut holder = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &script]).spawn().unwrap();
        let mut apps = Vec::new();
        for _ in 0..40 {
            apps = find_blocking_apps(std::slice::from_ref(&task), &[0]);
            if !apps.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let _ = holder.kill();
        let _ = holder.wait();

        assert_eq!(apps.len(), 1, "{apps:?}");
        assert_eq!(apps[0].exe, "powershell.exe");
        assert_eq!(apps[0].tasks, vec![0]);

        // Программа, которой нет в списке пункта, не предлагается, даже если держит файлы.
        let mut holder = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &script]).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        let other_task = CleanupTask { programs_to_close: &[Program { exe: "chrome.exe", helpers: &[] }], ..task };
        let apps = find_blocking_apps(std::slice::from_ref(&other_task), &[0]);
        let _ = holder.kill();
        let _ = holder.wait();
        assert!(apps.is_empty(), "{apps:?}");

        let _ = fs::remove_dir_all(&root);
    }
}
