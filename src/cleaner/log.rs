//! Журнал очистки: текстовый файл со всеми удалёнными файлами и ошибками.
//!
//! Журналы сохраняются в `%LOCALAPPDATA%\HexCleaner\logs`.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use super::report::{TaskReport, format_size};

pub struct CleanupLog {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl CleanupLog {
    /// Создаёт новый файл журнала с текущей датой и временем в имени.
    pub fn create() -> io::Result<Self> {
        Self::create_in(&logs_directory())
    }

    fn create_in(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;

        let now = chrono::Local::now();
        let path = dir.join(format!("cleanup_{}.log", now.format("%Y-%m-%d_%H-%M-%S")));
        let mut log = Self { writer: BufWriter::new(File::create(&path)?), path };

        log.line(&format!("HexCleaner {} — журнал очистки", env!("CARGO_PKG_VERSION")));
        log.line(&format!("Начало: {}", now.format("%d.%m.%Y %H:%M:%S")));
        Ok(log)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn line(&mut self, text: &str) {
        // Ошибка записи журнала не должна прерывать очистку.
        let _ = writeln!(self.writer, "{text}");
    }

    pub fn task_started(&mut self, title: &str) {
        self.line("");
        self.line(&format!("=== {title} ==="));
    }

    pub fn deleted(&mut self, path: &Path, size: u64) {
        self.line(&format!("  удалён     {:>10}  {}", format_size(size), path.display()));
    }

    pub fn failed(&mut self, path: &Path, error: &dyn std::fmt::Display) {
        self.line(&format!("  ошибка     {}: {error}", path.display()));
    }

    pub fn skipped(&mut self, path: &Path, reason: &str) {
        self.line(&format!("  пропущен   {}: {reason}", path.display()));
    }

    pub fn task_finished(&mut self, report: &TaskReport) {
        self.line(&format!("Итог: {}", report.summary()));
    }

    pub fn finish(&mut self, total: &TaskReport, cancelled: bool) {
        self.line("");
        if cancelled {
            self.line("Очистка остановлена пользователем.");
        }
        self.line(&format!(
            "ВСЕГО: удалено файлов {}, освобождено {}, не удалось удалить {}",
            total.deleted_files,
            format_size(total.freed_bytes),
            total.failed
        ));
        if total.freed_memory > 0 {
            self.line(&format!("Освобождено оперативной памяти: {}", format_size(total.freed_memory)));
        }
        self.line(&format!("Конец: {}", chrono::Local::now().format("%d.%m.%Y %H:%M:%S")));
        let _ = self.writer.flush();
    }
}

#[cfg(test)]
impl CleanupLog {
    pub fn for_tests(dir: &Path) -> Self {
        Self::create_in(dir).unwrap()
    }
}

pub fn logs_directory() -> PathBuf {
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_owned());
    Path::new(&local_app_data).join("HexCleaner").join("logs")
}
