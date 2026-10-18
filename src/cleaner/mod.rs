//! Движок очистки.
//!
//! Очистка выполняется в отдельном потоке, чтобы не подвешивать интерфейс.
//! Поток сообщает о ходе работы через канал событий [`CleanupEvent`],
//! а интерфейс может попросить его остановиться через [`CleanupJob::cancel`].
//!
//! Тот же движок выполняет и предварительный анализ ([`Mode::Analyze`]): обходит те же
//! папки по тем же правилам, но ничего не удаляет и не меняет, а только считает.

mod blockers;
mod failure;
mod files;
mod log;
mod paths;
mod report;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::catalog::{Action, CleanupTask};
use crate::system::delete::DeleteError;
use crate::system::restart_manager::{self, ClosedPrograms};
use crate::system::{event_log, memory, recycle_bin, services};
use crate::t;

pub use blockers::{BlockingApp, find_blocking_apps};
pub use log::logs_directory;
use log::CleanupLog;
pub use report::{Location, TaskReport, TaskResult, format_size};

/// Как часто отправлять в интерфейс промежуточные счётчики.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Сообщения от потока очистки к интерфейсу. `usize` — индекс пункта в каталоге.
pub enum CleanupEvent {
    /// Закрываются программы, мешающие очистке (может занять несколько секунд).
    ClosingPrograms,
    TaskStarted(usize),
    /// Промежуточные счётчики выполняющегося пункта.
    Progress(usize, TaskReport),
    TaskFinished(usize, TaskResult),
    /// Пункт пропущен, потому что у программы нет прав администратора.
    TaskSkipped(usize),
    Finished { total: TaskReport, log_path: PathBuf, cancelled: bool },
    /// Очистка не смогла начаться.
    Failed(String),
}

/// Что делать с найденным мусором.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Удалить.
    Clean,
    /// Только посчитать, сколько и где мусора. Ничего не удаляется и не меняется.
    Analyze,
}

/// Параметры запуска очистки.
#[derive(Clone)]
pub struct CleanupOptions {
    pub mode: Mode,
    /// Есть ли у программы права администратора. Без них пункты, требующие этих прав, пропускаются.
    pub is_admin: bool,
    /// Сжать журналы прошлых очисток в zip-архивы перед созданием нового журнала.
    pub compress_old_logs: bool,
    /// Программы (имена файлов, например `chrome.exe`), которые нужно корректно закрыть
    /// перед очисткой. Обычно это [`find_blocking_apps`]. При анализе не закрываются.
    pub close_programs: Vec<String>,
    /// Запустить закрытые программы снова после очистки.
    pub restart_programs: bool,
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

    /// Ждёт следующее событие не дольше `timeout` (для командной строки, где нет цикла окна).
    /// `Disconnected` означает, что поток очистки завершился, не прислав `Finished`.
    pub fn wait_event(&self, timeout: Duration) -> Result<CleanupEvent, RecvTimeoutError> {
        self.events.recv_timeout(timeout)
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
    let mut log = match CleanupLog::create(options.mode, options.compress_old_logs) {
        Ok(log) => log,
        Err(error) => {
            let _ = events.send(CleanupEvent::Failed(t!("engine.log_failed", error = error)));
            return;
        }
    };
    log.line(&if is_admin { t!("log.admin_yes") } else { t!("log.admin_no") });

    let closed_programs = if options.mode == Mode::Clean && !options.close_programs.is_empty() {
        let _ = events.send(CleanupEvent::ClosingPrograms);
        close_programs(&options.close_programs, &mut log)
    } else {
        None
    };

    let mut total = TaskReport::default();
    for &index in selected {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let task = &tasks[index];
        log.task_started(&task.title());

        if task.requires_admin && !is_admin {
            log.line(&t!("log.skipped_needs_admin"));
            let _ = events.send(CleanupEvent::TaskSkipped(index));
            continue;
        }

        let _ = events.send(CleanupEvent::TaskStarted(index));
        let mut run = TaskRun::new(index, options.mode, &mut log, events, cancel);
        run.execute(task);
        let result = run.into_result();

        log.task_finished(&result);
        total += result.report;
        let _ = events.send(CleanupEvent::TaskFinished(index, result));
    }

    if options.restart_programs
        && let Some(closed) = &closed_programs
    {
        log.line("");
        match closed.restart() {
            Ok(()) => log.line(&t!("log.programs_restarted")),
            Err(error) => log.line(&t!("log.programs_restart_failed", error = error)),
        }
    }

    let cancelled = cancel.load(Ordering::Relaxed);
    log.finish(&total, cancelled);
    let _ = events.send(CleanupEvent::Finished { total, log_path: log.path().to_owned(), cancelled });
}

/// Корректно закрывает программы, мешающие очистке, и пишет результат в журнал.
fn close_programs(exes: &[String], log: &mut CleanupLog) -> Option<ClosedPrograms> {
    log.line("");
    log.line(&t!("log.closing_programs", programs = exes.join(", ")));
    match restart_manager::close_programs(exes) {
        Ok(closed) => {
            if closed.still_running.is_empty() {
                log.line(&t!("log.programs_closed"));
            } else {
                log.line(&t!("log.programs_still_running", programs = closed.still_running.join(", ")));
            }
            Some(closed)
        }
        Err(error) => {
            log.line(&t!("log.programs_close_failed", error = error));
            None
        }
    }
}

/// Выполнение одного пункта: счётчики, журнал и флаг отмены в одном месте.
pub struct TaskRun<'a> {
    task_index: usize,
    mode: Mode,
    report: TaskReport,
    /// Разбивка по папкам; `current_location` — папка, которая обрабатывается сейчас.
    locations: Vec<Location>,
    current_location: Option<usize>,
    note: Option<String>,
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
        mode: Mode,
        log: &'a mut CleanupLog,
        events: &'a Sender<CleanupEvent>,
        cancel: &'a AtomicBool,
    ) -> Self {
        Self {
            task_index,
            mode,
            report: TaskReport::default(),
            locations: Vec::new(),
            current_location: None,
            note: None,
            log,
            events,
            cancel,
            last_progress: Instant::now(),
            lock_queries_left: failure::LOCK_QUERIES_PER_TASK,
        }
    }

    fn execute(&mut self, task: &CleanupTask) {
        // При анализе службы не трогаем: файлы только читаются.
        let stopped_services = match self.mode {
            Mode::Clean => self.stop_services(task.services_to_stop),
            Mode::Analyze => Vec::new(),
        };

        for action in &task.actions {
            if self.is_cancelled() {
                break;
            }
            self.execute_action(action);
        }

        self.start_services(&stopped_services);
    }

    /// Итог пункта: пустые папки отбрасываются, остальные сортируются по объёму.
    fn into_result(self) -> TaskResult {
        let mut locations = self.locations;
        locations.retain(|location| location.files > 0 || location.bytes > 0);
        locations.sort_by_key(|location| std::cmp::Reverse(location.bytes));
        TaskResult { report: self.report, locations, note: self.note }
    }

    fn execute_action(&mut self, action: &Action) {
        match action {
            Action::ClearDirectory { pattern, min_age } => {
                for dir in self.safe_directories(pattern) {
                    self.enter_location(&dir);
                    files::clear_directory(self, &dir, *min_age);
                }
            }
            Action::DeleteDirectory { pattern } => {
                for dir in self.safe_directories(pattern) {
                    if let Ok(metadata) = dir.symlink_metadata() {
                        self.enter_location(&dir);
                        files::delete_directory(self, &dir, &metadata);
                    }
                }
            }
            Action::DeleteFiles { pattern } => {
                for path in paths::expand(pattern) {
                    let Ok(metadata) = path.symlink_metadata() else { continue };
                    if metadata.is_file() && !files::is_link(&metadata) {
                        self.enter_location(path.parent().unwrap_or(&path));
                        files::delete_file(self, &path, &metadata);
                    }
                }
            }
            Action::ClearEventLogs => self.clear_event_logs(),
            Action::TrimProcessMemory => self.trim_process_memory(),
            Action::EmptyRecycleBin => self.empty_recycle_bin(),
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
                self.skipped(&dir, &t!("skip.link"));
            } else if paths::is_protected(&dir) {
                self.skipped(&dir, &t!("skip.protected"));
            } else {
                result.push(dir);
            }
        }
        result
    }

    fn clear_event_logs(&mut self) {
        let logs_dir = Path::new(&std::env::var("WINDIR").unwrap_or_default()).join(r"System32\winevt\Logs");
        self.enter_location(&logs_dir);
        if self.is_analysis() {
            self.estimate_event_logs(&logs_dir);
            return;
        }

        let size_before = files::directory_size(&logs_dir);

        let mut cleared = 0;
        let mut not_cleared = 0;
        let result = event_log::clear_all(|channel, result| match result {
            Ok(()) => {
                cleared += 1;
                self.log.line(&t!("log.event_log_cleared", channel = channel));
            }
            // Аналитические и отладочные журналы нельзя очистить, пока они включены, — это нормально.
            Err(_) => not_cleared += 1,
        });
        if let Err(error) = result {
            self.log.line(&t!("log.event_logs_list_failed", error = error));
        }

        let size_after = files::directory_size(&logs_dir);
        self.add_freed_bytes(size_before.saturating_sub(size_after));
        self.log.line(&t!("log.event_logs_summary", cleared = cleared, skipped = not_cleared));
    }

    /// Оценка для анализа: очищенный журнал событий ужимается до минимального размера
    /// файла (68 КБ), всё сверх этого освободится.
    fn estimate_event_logs(&mut self, logs_dir: &Path) {
        const CLEARED_LOG_SIZE: u64 = 68 * 1024;
        let reclaimable = files::file_sizes(logs_dir, "evtx")
            .into_iter()
            .map(|size| size.saturating_sub(CLEARED_LOG_SIZE))
            .sum();
        self.add_freed_bytes(reclaimable);
    }

    fn trim_process_memory(&mut self) {
        if self.is_analysis() {
            self.note = Some(t!("note.memory_in_analysis"));
            return;
        }
        let result = memory::trim_all_working_sets();
        self.report.freed_memory += result.freed_bytes;
        self.log.line(&t!(
            "log.memory_summary",
            trimmed = result.trimmed,
            inaccessible = result.inaccessible,
            size = format_size(result.freed_bytes),
        ));
    }

    /// Корзина. При анализе только узнаём, сколько в ней лежит.
    fn empty_recycle_bin(&mut self) {
        // Специальная папка оболочки: клик по ней в разбивке откроет корзину в проводнике.
        self.enter_location(Path::new("shell:RecycleBinFolder"));
        let contents = match recycle_bin::query() {
            Ok(contents) => contents,
            Err(error) => {
                self.log.line(&t!("log.recycle_bin_failed", error = error));
                return;
            }
        };
        if contents.items == 0 {
            return;
        }

        if !self.is_analysis() {
            if let Err(error) = recycle_bin::empty() {
                self.report.failed += 1;
                self.log.line(&t!("log.recycle_bin_failed", error = error));
                return;
            }
            let size = format_size(contents.bytes);
            self.log.line(&t!("log.recycle_bin_emptied", files = contents.items, size = size));
        }
        self.record_files(contents.items, contents.bytes);
    }

    /// Останавливает запущенные службы и возвращает их имена, чтобы потом запустить снова.
    fn stop_services(&mut self, names: &[&'static str]) -> Vec<&'static str> {
        let mut stopped = Vec::new();
        for &name in names {
            match services::stop(name) {
                Ok(true) => {
                    self.log.line(&t!("log.service_stopped", name = name));
                    stopped.push(name);
                }
                Ok(false) => {}
                Err(error) => self.log.line(&t!("log.service_stop_failed", name = name, error = error)),
            }
        }
        stopped
    }

    fn start_services(&mut self, names: &[&str]) {
        for &name in names {
            match services::start(name) {
                Ok(()) => self.log.line(&t!("log.service_started", name = name)),
                Err(error) => self.log.line(&t!("log.service_start_failed", name = name, error = error)),
            }
        }
    }

    // --- Методы, через которые `files` сообщает о результатах ---

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn is_analysis(&self) -> bool {
        self.mode == Mode::Analyze
    }

    /// Дальнейшие файлы учитываются в разбивке по папкам под этой папкой.
    fn enter_location(&mut self, path: &Path) {
        let index = match self.locations.iter().position(|location| location.path == path) {
            Some(index) => index,
            None => {
                self.locations.push(Location { path: path.to_owned(), files: 0, bytes: 0 });
                self.locations.len() - 1
            }
        };
        self.current_location = Some(index);
    }

    fn add_freed_bytes(&mut self, bytes: u64) {
        self.report.freed_bytes += bytes;
        if let Some(index) = self.current_location {
            self.locations[index].bytes += bytes;
        }
    }

    /// Учитывает файл: удалённый при очистке или найденный при анализе.
    fn record_file(&mut self, path: &Path, size: u64) {
        // При анализе по каждому файлу журнал не пишем — только итоги по папкам.
        if !self.is_analysis() {
            self.log.deleted(path, size);
        }
        self.record_files(1, size);
    }

    /// Учитывает сразу несколько файлов общим размером `bytes`.
    fn record_files(&mut self, count: u64, bytes: u64) {
        self.report.deleted_files += count;
        self.add_freed_bytes(bytes);
        if let Some(index) = self.current_location {
            self.locations[index].files += count;
        }

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
        let mut run = TaskRun::new(0, Mode::Clean, &mut log, &sender, &cancel);

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

    /// Анализ ничего не удаляет и не меняет, но находит ровно то, что потом удалит очистка,
    /// с разбивкой по папкам.
    #[test]
    fn analysis_counts_without_deleting() {
        let root = std::env::temp_dir().join(format!("hexcleaner_analysis_{}", std::process::id()));
        let big = root.join("big_cache");
        let small = root.join("small_cache");
        fs::create_dir_all(big.join("nested")).unwrap();
        fs::create_dir_all(&small).unwrap();
        fs::write(big.join("a.bin"), vec![0u8; 3000]).unwrap();
        fs::write(big.join(r"nested\b.bin"), vec![0u8; 1000]).unwrap();
        fs::write(small.join("c.bin"), vec![0u8; 10]).unwrap();
        let readonly = small.join("c.bin");
        let mut permissions = fs::metadata(&readonly).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&readonly, permissions).unwrap();

        let action = Action::ClearDirectory { pattern: format!(r"{}\*_cache", root.display()), min_age: None };
        let (sender, _events) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        let mut log = CleanupLog::for_tests(&root.join("logs"));

        let mut analysis = TaskRun::new(0, Mode::Analyze, &mut log, &sender, &cancel);
        analysis.execute_action(&action);
        let found = analysis.into_result();

        assert_eq!(found.report.deleted_files, 3);
        assert_eq!(found.report.freed_bytes, 4010);
        assert!(big.join(r"nested\b.bin").exists(), "анализ удалил файл!");
        assert!(fs::metadata(&readonly).unwrap().permissions().readonly(), "анализ изменил атрибуты!");
        // Папки отсортированы по объёму: сначала самая большая.
        let locations: Vec<_> = found.locations.iter().map(|l| (l.path.clone(), l.files, l.bytes)).collect();
        assert_eq!(locations, vec![(big.clone(), 2, 4000), (small.clone(), 1, 10)]);

        let mut cleanup = TaskRun::new(0, Mode::Clean, &mut log, &sender, &cancel);
        cleanup.execute_action(&action);
        let cleaned = cleanup.into_result();

        assert_eq!(cleaned.report.deleted_files, found.report.deleted_files);
        assert_eq!(cleaned.report.freed_bytes, found.report.freed_bytes);
        assert!(!big.join("nested").exists());
        assert!(!readonly.exists());

        drop(log);
        let _ = fs::remove_dir_all(&root);
    }
}
