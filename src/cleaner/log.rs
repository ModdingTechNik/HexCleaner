//! Журнал очистки: текстовый файл со всеми удалёнными файлами и ошибками.
//! Анализ тоже пишет журнал (отчёт): в нём только итоги по папкам, без списка файлов.
//!
//! Журналы сохраняются в `%LOCALAPPDATA%\HexCleaner\logs`. Журнал текущей очистки
//! остаётся обычным текстом, а журналы прошлых очисток сжимаются в zip-архивы
//! (по одному архиву на журнал) — так они занимают в 10–20 раз меньше места.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{Datelike, Timelike};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::Mode;
use super::report::{TaskReport, TaskResult, format_size};

pub struct CleanupLog {
    path: PathBuf,
    writer: BufWriter<File>,
    mode: Mode,
}

impl CleanupLog {
    /// Создаёт новый файл журнала с текущей датой и временем в имени.
    /// Если `compress_previous` — журналы прошлых очисток предварительно сжимаются.
    pub fn create(mode: Mode, compress_previous: bool) -> io::Result<Self> {
        Self::create_in(&logs_directory(), mode, compress_previous)
    }

    fn create_in(dir: &Path, mode: Mode, compress_previous: bool) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        if compress_previous {
            compress_previous_logs(dir);
        }

        let now = chrono::Local::now();
        let (prefix, title) = match mode {
            Mode::Clean => ("cleanup", "журнал очистки"),
            Mode::Analyze => ("analysis", "отчёт анализа (ничего не удалялось)"),
        };
        let path = dir.join(format!("{prefix}_{}.log", now.format("%Y-%m-%d_%H-%M-%S")));
        let mut log = Self { writer: BufWriter::new(File::create(&path)?), path, mode };

        log.line(&format!("HexCleaner {} — {title}", env!("CARGO_PKG_VERSION")));
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

    pub fn task_finished(&mut self, result: &TaskResult) {
        if let Some(note) = result.note {
            self.line(&format!("  {note}"));
        }
        for location in &result.locations {
            self.line(&format!(
                "  папка   {:>10}  {:>6} файлов  {}",
                format_size(location.bytes),
                location.files,
                location.path.display()
            ));
        }
        let prefix = match self.mode {
            Mode::Clean => "Итог",
            Mode::Analyze => "Найдено",
        };
        self.line(&format!("{prefix}: {}", result.report.summary()));
    }

    pub fn finish(&mut self, total: &TaskReport, cancelled: bool) {
        self.line("");
        if cancelled {
            self.line("Остановлено пользователем.");
        }
        match self.mode {
            Mode::Clean => self.line(&format!(
                "ВСЕГО: удалено файлов {}, освобождено {}, не удалось удалить {}",
                total.deleted_files,
                format_size(total.freed_bytes),
                total.failed
            )),
            Mode::Analyze => self.line(&format!(
                "ВСЕГО: найдено файлов {}, можно освободить {}",
                total.deleted_files,
                format_size(total.freed_bytes)
            )),
        }
        if total.freed_memory > 0 {
            self.line(&format!("Освобождено оперативной памяти: {}", format_size(total.freed_memory)));
        }
        self.line(&format!("Конец: {}", chrono::Local::now().format("%d.%m.%Y %H:%M:%S")));
        let _ = self.writer.flush();
    }
}

/// Сжимает журналы прошлых очисток: `cleanup_….log` → `cleanup_….zip`.
/// Ошибка сжатия не мешает очистке — журнал просто останется несжатым.
fn compress_previous_logs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let logs = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("log")));

    for log_path in logs {
        let zip_path = log_path.with_extension("zip");
        match compress_file(&log_path, &zip_path) {
            Ok(()) => {
                let _ = fs::remove_file(&log_path);
            }
            Err(_) => {
                // Недописанный архив не нужен: оригинал журнала остаётся на месте.
                let _ = fs::remove_file(&zip_path);
            }
        }
    }
}

/// Упаковывает один файл в zip-архив, сохраняя его имя и дату изменения.
fn compress_file(source: &Path, zip_path: &Path) -> zip::result::ZipResult<()> {
    let name = source.file_name().unwrap_or_default().to_string_lossy();
    let modified: chrono::DateTime<chrono::Local> = fs::metadata(source)?.modified()?.into();
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(zip_date_time(&modified));

    let mut zip = ZipWriter::new(File::create(zip_path)?);
    zip.start_file(name, options)?;
    io::copy(&mut File::open(source)?, &mut zip)?;
    zip.finish()?;
    Ok(())
}

fn zip_date_time(time: &chrono::DateTime<chrono::Local>) -> zip::DateTime {
    zip::DateTime::from_date_and_time(
        time.year() as u16,
        time.month() as u8,
        time.day() as u8,
        time.hour() as u8,
        time.minute() as u8,
        time.second() as u8,
    )
    .unwrap_or_default()
}

#[cfg(test)]
impl CleanupLog {
    pub fn for_tests(dir: &Path) -> Self {
        Self::create_in(dir, Mode::Clean, true).unwrap()
    }
}

pub fn logs_directory() -> PathBuf {
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_owned());
    Path::new(&local_app_data).join("HexCleaner").join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn previous_logs_are_compressed() {
        let dir = std::env::temp_dir().join(format!("hexcleaner_logs_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let old_log = dir.join("cleanup_2020-01-01_00-00-00.log");
        let text = "  удалён     1.0 КБ  C:\\Temp\\file.tmp\n".repeat(1000);
        fs::write(&old_log, &text).unwrap();

        let new_log = CleanupLog::for_tests(&dir);

        // Старый журнал заменён архивом, новый остался обычным текстовым файлом.
        assert!(!old_log.exists());
        assert!(new_log.path().exists());
        let zip_path = old_log.with_extension("zip");
        assert!(fs::metadata(&zip_path).unwrap().len() < text.len() as u64 / 10);

        let mut archive = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        let mut unpacked = String::new();
        archive.by_name("cleanup_2020-01-01_00-00-00.log").unwrap().read_to_string(&mut unpacked).unwrap();
        assert_eq!(unpacked, text);

        drop(new_log);
        let _ = fs::remove_dir_all(&dir);
    }
}
