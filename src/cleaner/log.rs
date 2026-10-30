//! Журнал очистки: текстовый файл со всеми удалёнными файлами и ошибками.
//! Анализ тоже пишет журнал (отчёт): в нём только итоги по папкам, без списка файлов.
//!
//! Журналы сохраняются в `%LOCALAPPDATA%\HexCleaner\logs`. Журнал текущей очистки
//! остаётся обычным текстом, а журналы прошлых очисток сжимаются: в zip-архивы
//! (по одному архиву на журнал, в 10–20 раз меньше места) или сжатием NTFS всей папки
//! (журналы остаются обычными файлами).

use std::fs::{self, File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{Datelike, Timelike};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::Mode;
use super::report::{TaskReport, TaskResult, format_size};
use crate::settings::Compression;
use crate::system::compact;
use crate::t;

pub struct CleanupLog {
    path: PathBuf,
    writer: BufWriter<File>,
    mode: Mode,
}

impl CleanupLog {
    /// Создаёт новый файл журнала с текущей датой и временем в имени.
    /// С `compress_previous` журналы прошлых очисток предварительно сжимаются этим способом.
    pub fn create(mode: Mode, compress_previous: Option<Compression>) -> io::Result<Self> {
        Self::create_in(&logs_directory(), mode, compress_previous)
    }

    fn create_in(dir: &Path, mode: Mode, compress_previous: Option<Compression>) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        match compress_previous {
            Some(Compression::Zip) => compress_previous_logs(dir),
            // Папка журналов помечается сжатой: старые и новые журналы сжимаются сами.
            Some(Compression::Ntfs) => {
                let _ = compact::compress_in_background(dir);
            }
            None => {}
        }

        let now = chrono::Local::now();
        let version = env!("CARGO_PKG_VERSION");
        let (prefix, title) = match mode {
            Mode::Clean => ("cleanup", t!("log.title_clean", version = version)),
            Mode::Analyze => ("analysis", t!("log.title_analysis", version = version)),
        };
        let (path, file) = new_log_file(dir, &format!("{prefix}_{}", now.format("%Y-%m-%d_%H-%M-%S")))?;
        let mut log = Self { writer: BufWriter::new(file), path, mode };

        log.line(&title);
        log.line(&t!("log.started", time = now_text()));
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
        let size = format!("{:>10}", format_size(size));
        self.line(&t!("log.deleted", size = size, path = path.display()));
    }

    pub fn trashed(&mut self, path: &Path, size: u64) {
        let size = format!("{:>10}", format_size(size));
        self.line(&t!("log.trashed", size = size, path = path.display()));
    }

    pub fn failed(&mut self, path: &Path, reason: &str) {
        self.line(&t!("log.failed", path = path.display(), reason = reason));
    }

    pub fn skipped(&mut self, path: &Path, reason: &str) {
        self.line(&t!("log.skipped", path = path.display(), reason = reason));
    }

    pub fn task_finished(&mut self, result: &TaskResult) {
        if let Some(note) = &result.note {
            self.line(&format!("  {note}"));
        }
        for location in &result.locations {
            let size = format!("{:>10}", format_size(location.bytes));
            let files = format!("{:>6}", location.files);
            self.line(&t!("log.location", size = size, files = files, path = location.path.display()));
        }
        let summary = result.report.summary();
        self.line(&match self.mode {
            Mode::Clean => t!("log.task_total", summary = summary),
            Mode::Analyze => t!("log.task_found", summary = summary),
        });
    }

    pub fn finish(&mut self, total: &TaskReport, cancelled: bool) {
        self.line("");
        if cancelled {
            self.line(&t!("log.cancelled"));
        }
        let size = format_size(total.freed_bytes);
        self.line(&match self.mode {
            Mode::Clean => t!("log.total_clean", files = total.deleted_files, size = size, failed = total.failed),
            Mode::Analyze => t!("log.total_analysis", files = total.deleted_files, size = size),
        });
        if total.freed_memory > 0 {
            self.line(&t!("log.total_memory", size = format_size(total.freed_memory)));
        }
        if total.trashed_files > 0 {
            let size = format_size(total.trashed_bytes);
            self.line(&t!("log.total_trash", files = total.trashed_files, size = size));
        }
        self.line(&t!("log.finished", time = now_text()));
        let _ = self.writer.flush();
    }
}

/// Текущие дата и время в формате языка интерфейса.
fn now_text() -> String {
    chrono::Local::now().format(&t!("log.datetime_format")).to_string()
}

/// Сжимает журналы прошлых очисток: `cleanup_….log` → `cleanup_….zip`.
/// Ошибка сжатия не мешает очистке — журнал просто останется несжатым.
/// Новый файл журнала `<name>.log`. Если имя занято — очистка в ту же секунду, например два
/// задания планировщика на одно время, — добавляется номер: `<name>_2.log`. Занятым считается
/// и имя уже упакованного журнала (`<name>.zip`). Файл создаётся, только если его ещё нет,
/// поэтому две одновременные очистки не запишут журналы в один файл.
fn new_log_file(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    let mut number = 1;
    loop {
        let numbered = if number == 1 { name.to_owned() } else { format!("{name}_{number}") };
        if !dir.join(format!("{numbered}.zip")).exists() {
            let path = dir.join(format!("{numbered}.log"));
            match File::create_new(&path) {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
                Err(_) => {}
            }
        }
        number += 1;
    }
}

/// Журнал ещё пишется — его держит идущая сейчас очистка (например, другое задание
/// планировщика на то же время). Такой журнал не упаковываем: он ещё не закончен.
fn is_in_use(path: &Path) -> bool {
    // Без общего доступа файл не откроется, пока его держит кто-то ещё.
    OpenOptions::new().read(true).share_mode(0).open(path).is_err()
}

fn compress_previous_logs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let logs = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("log")));

    for log_path in logs.filter(|path| !is_in_use(path)) {
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
        Self::create_in(dir, Mode::Clean, Some(Compression::Zip)).unwrap()
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

    /// Журнал идущей очистки не упаковывается, пока она его пишет.
    #[test]
    fn log_in_use_is_not_compressed() {
        let dir = std::env::temp_dir().join(format!("hexcleaner_log_in_use_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let running = dir.join("cleanup_2026-10-30_03-00-00.log");
        let writer = File::create(&running).unwrap();

        compress_previous_logs(&dir);
        assert!(running.exists() && !running.with_extension("zip").exists());

        drop(writer);
        compress_previous_logs(&dir);
        assert!(!running.exists() && running.with_extension("zip").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Две очистки в одну секунду не пишут в один журнал, даже если первый уже упакован в zip.
    #[test]
    fn logs_of_the_same_second_get_numbers() {
        let dir = std::env::temp_dir().join(format!("hexcleaner_log_names_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cleanup_2026-10-30_03-00-00.zip"), "").unwrap();

        let (first, _first_file) = new_log_file(&dir, "cleanup_2026-10-30_03-00-00").unwrap();
        let (second, _second_file) = new_log_file(&dir, "cleanup_2026-10-30_03-00-00").unwrap();
        assert!(first.ends_with("cleanup_2026-10-30_03-00-00_2.log"), "{}", first.display());
        assert!(second.ends_with("cleanup_2026-10-30_03-00-00_3.log"), "{}", second.display());

        drop((_first_file, _second_file));
        let _ = fs::remove_dir_all(&dir);
    }
}
