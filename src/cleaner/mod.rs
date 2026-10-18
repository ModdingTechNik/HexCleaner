//! Движок очистки.
//!
//! Очистка выполняется в отдельном потоке, чтобы не подвешивать интерфейс.
//! Поток сообщает о ходе работы через канал событий [`CleanupEvent`],
//! а интерфейс может попросить его остановиться через [`CleanupJob::cancel`].

mod failure;
mod files;
mod log;
mod paths;
mod report;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::catalog::{Action, CleanupTask};
use crate::system::delete::DeleteError;
use crate::system::{event_log, memory, services};

pub use log::logs_directory;
use log::CleanupLog;
pub use report::{TaskReport, format_size};

/// Как часто отправлять в интерфейс промежуточные счётчики.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Сообщения от потока очистки к интерфейсу. `usize` — индекс пункта в каталоге.
pub enum CleanupEvent {
    TaskStarted(usize),
    /// Промежуточные счётчики выполняющегося пункта.
    Progress(usize, TaskReport),
    TaskFinished(usize, TaskReport),
    /// Пункт пропущен, потому что у программы нет прав администратора.
    TaskSkipped(usize),
    Finished { total: TaskReport, log_path: PathBuf, cancelled: bool },
    /// Очистка не смогла начаться.
    Failed(String),
}

/// Параметры запуска очистки.
#[derive(Clone, Copy)]
pub struct CleanupOptions {
    /// Есть ли у программы права администратора. Без них пункты, требующие этих прав, пропускаются.
    pub is_admin: bool,
    /// Сжать журналы прошлых очисток в zip-архивы перед созданием нового журнала.
    pub compress_old_logs: bool,
}

/// Запущенная очистка.
pub struct CleanupJob {
    events: Receiver<CleanupEvent>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl CleanupJob {
    /// Запускает очистку выбранных пунктов (`selected` — индексы в `tasks`).
    pub fn start(tasks: Arc<Vec<CleanupTask>>, selected: Vec<usize>, options: CleanupOptions) -> Self {
        let (sender, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let cancel_flag = Arc::clone(&cancel);
        let worker = thread::spawn(move || run_cleanup(&tasks, &selected, options, &sender, &cancel_flag));

        Self { events, cancel, worker: Some(worker) }
    }

    /// Просит поток очистки остановиться как можно скорее.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Все события, накопившиеся с прошлого вызова.
    pub fn poll_events(&self) -> Vec<CleanupEvent> {
        self.events.try_iter().collect()
    }
}

impl Drop for CleanupJob {
    /// Если окно закрывают посреди очистки, останавливаем поток и дожидаемся его,
    /// чтобы остановленные службы Windows были запущены обратно.
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_cleanup(
    tasks: &[CleanupTask],
    selected: &[usize],
    options: CleanupOptions,
    events: &Sender<CleanupEvent>,
    cancel: &AtomicBool,
) {
    let is_admin = options.is_admin;
    let mut log = match CleanupLog::create(options.compress_old_logs) {
        Ok(log) => log,
        Err(error) => {
            let _ = events.send(CleanupEvent::Failed(format!("Не удалось создать журнал очистки: {error}")));
            return;
        }
    };
    log.line(if is_admin {
        "Права администратора: есть"
    } else {
        "Права администратора: нет — пункты, требующие их, будут пропущены"
    });

    let mut total = TaskReport::default();
    for &index in selected {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let task = &tasks[index];
        log.task_started(task.title);

        if task.requires_admin && !is_admin {
            log.line("Пропущено: нужны права администратора");
            let _ = events.send(CleanupEvent::TaskSkipped(index));
            continue;
        }

        let _ = events.send(CleanupEvent::TaskStarted(index));
        let mut run = TaskRun::new(index, &mut log, events, cancel);
        run.execute(task);
        let report = run.report;

        log.task_finished(&report);
        total += report;
        let _ = events.send(CleanupEvent::TaskFinished(index, report));
    }

    let cancelled = cancel.load(Ordering::Relaxed);
    log.finish(&total, cancelled);
    let _ = events.send(CleanupEvent::Finished { total, log_path: log.path().to_owned(), cancelled });
}

/// Выполнение одного пункта: счётчики, журнал и флаг отмены в одном месте.
pub struct TaskRun<'a> {
    task_index: usize,
    report: TaskReport,
    log: &'a mut CleanupLog,
    events: &'a Sender<CleanupEvent>,
    cancel: &'a AtomicBool,
    last_progress: Instant,
    /// Сколько ещё раз можно узнавать, какой процесс держит файл (см. `failure`).
    lock_queries_left: u32,
}

impl<'a> TaskRun<'a> {
    fn new(
        task_index: usize,
        log: &'a mut CleanupLog,
        events: &'a Sender<CleanupEvent>,
        cancel: &'a AtomicBool,
    ) -> Self {
        Self {
            task_index,
            report: TaskReport::default(),
            log,
            events,
            cancel,
            last_progress: Instant::now(),
            lock_queries_left: failure::LOCK_QUERIES_PER_TASK,
        }
    }

    fn execute(&mut self, task: &CleanupTask) {
        let stopped_services = self.stop_services(task.services_to_stop);

        for action in &task.actions {
            if self.is_cancelled() {
                break;
            }
            self.execute_action(action);
        }

        self.start_services(&stopped_services);
    }

    fn execute_action(&mut self, action: &Action) {
        match action {
            Action::ClearDirectory { pattern, min_age } => {
                for dir in self.safe_directories(pattern) {
                    files::clear_directory(self, &dir, *min_age);
                }
            }
            Action::DeleteDirectory { pattern } => {
                for dir in self.safe_directories(pattern) {
                    if let Ok(metadata) = dir.symlink_metadata() {
                        files::delete_directory(self, &dir, &metadata);
                    }
                }
            }
            Action::DeleteFiles { pattern } => {
                for path in paths::expand(pattern) {
                    let Ok(metadata) = path.symlink_metadata() else { continue };
                    if metadata.is_file() && !files::is_link(&metadata) {
                        files::delete_file(self, &path, &metadata);
                    }
                }
            }
            Action::ClearEventLogs => self.clear_event_logs(),
            Action::TrimProcessMemory => self.trim_process_memory(),
        }
    }

    /// Папки по шаблону, которые действительно можно очищать:
    /// существуют, не являются ссылками и не входят в список защищённых.
    fn safe_directories(&mut self, pattern: &str) -> Vec<PathBuf> {
        let mut result = Vec::new();
        for dir in paths::expand(pattern) {
            let Ok(metadata) = dir.symlink_metadata() else { continue };
            if !metadata.is_dir() {
                continue;
            }
            if files::is_link(&metadata) {
                self.skipped(&dir, "ссылка на другую папку, не трогаем");
            } else if paths::is_protected(&dir) {
                self.skipped(&dir, "защищённая папка, не трогаем");
            } else {
                result.push(dir);
            }
        }
        result
    }

    fn clear_event_logs(&mut self) {
        let logs_dir = Path::new(&std::env::var("WINDIR").unwrap_or_default()).join(r"System32\winevt\Logs");
        let size_before = files::directory_size(&logs_dir);

        let mut cleared = 0;
        let mut not_cleared = 0;
        let result = event_log::clear_all(|channel, result| match result {
            Ok(()) => {
                cleared += 1;
                self.log.line(&format!("  очищен журнал событий  {channel}"));
            }
            // Аналитические и отладочные журналы нельзя очистить, пока они включены, — это нормально.
            Err(_) => not_cleared += 1,
        });
        if let Err(error) = result {
            self.log.line(&format!("  не удалось получить список журналов событий: {error}"));
        }

        let size_after = files::directory_size(&logs_dir);
        self.report.freed_bytes += size_before.saturating_sub(size_after);
        self.log.line(&format!(
            "  журналов событий очищено: {cleared}, пропущено (активные или защищённые): {not_cleared}"
        ));
    }

    fn trim_process_memory(&mut self) {
        let result = memory::trim_all_working_sets();
        self.report.freed_memory += result.freed_bytes;
        self.log.line(&format!(
            "  обработано процессов: {}, недоступно: {}, освобождено: {}",
            result.trimmed,
            result.inaccessible,
            format_size(result.freed_bytes)
        ));
    }

    /// Останавливает запущенные службы и возвращает их имена, чтобы потом запустить снова.
    fn stop_services(&mut self, names: &[&'static str]) -> Vec<&'static str> {
        let mut stopped = Vec::new();
        for &name in names {
            match services::stop(name) {
                Ok(true) => {
                    self.log.line(&format!("  служба {name} остановлена"));
                    stopped.push(name);
                }
                Ok(false) => {}
                Err(error) => self.log.line(&format!("  не удалось остановить службу {name}: {error}")),
            }
        }
        stopped
    }

    fn start_services(&mut self, names: &[&str]) {
        for &name in names {
            match services::start(name) {
                Ok(()) => self.log.line(&format!("  служба {name} запущена")),
                Err(error) => self.log.line(&format!("  не удалось запустить службу {name}: {error}")),
            }
        }
    }

    // --- Методы, через которые `files` сообщает о результатах ---

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn file_deleted(&mut self, path: &Path, size: u64) {
        self.report.deleted_files += 1;
        self.report.freed_bytes += size;
        self.log.deleted(path, size);

        if self.last_progress.elapsed() >= PROGRESS_INTERVAL {
            self.last_progress = Instant::now();
            let _ = self.events.send(CleanupEvent::Progress(self.task_index, self.report));
        }
    }

    /// Файл не удалось удалить: считаем ошибку и пишем в журнал понятную причину.
    fn delete_failed(&mut self, path: &Path, error: &DeleteError) {
        self.report.failed += 1;
        let reason = failure::explain(path, error, &mut self.lock_queries_left);
        self.log.failed(path, &reason);
    }

    fn skipped(&mut self, path: &Path, reason: &str) {
        self.log.skipped(path, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// Очистка папки удаляет файлы и вложенные папки, но не проходит по ссылке (junction)
    /// и не трогает файлы, на которые она указывает.
    #[test]
    fn clear_directory_does_not_follow_junctions() {
        let root = std::env::temp_dir().join(format!("hexcleaner_test_{}", std::process::id()));
        let junk = root.join("junk");
        let user_files = root.join("user_files");
        fs::create_dir_all(junk.join(r"nested\deeper")).unwrap();
        fs::create_dir_all(&user_files).unwrap();
        fs::write(junk.join("a.tmp"), "12345").unwrap();
        fs::write(junk.join(r"nested\deeper\b.tmp"), "123").unwrap();
        fs::write(user_files.join("document.txt"), "important").unwrap();

        let link = junk.join("link_to_user_files");
        let status = Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&user_files).output().unwrap();
        assert!(status.status.success(), "не удалось создать junction");

        let (sender, _events) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        let mut log = CleanupLog::for_tests(&root.join("logs"));
        let mut run = TaskRun::new(0, &mut log, &sender, &cancel);

        files::clear_directory(&mut run, &junk, None);

        assert_eq!(run.report.deleted_files, 2);
        assert_eq!(run.report.freed_bytes, 8);
        assert!(!junk.join("nested").exists());
        assert!(link.exists(), "ссылка должна остаться нетронутой");
        assert!(user_files.join("document.txt").exists(), "файлы за ссылкой удалены!");

        drop(log);
        let _ = fs::remove_dir(&link);
        let _ = fs::remove_dir_all(&root);
    }
}
