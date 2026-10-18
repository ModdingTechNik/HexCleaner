//! Статистика очистки.

use std::ops::AddAssign;
use std::path::PathBuf;

use crate::t;

/// Сколько мусора найдено (или удалено) в одной папке.
#[derive(Clone, Debug)]
pub struct Location {
    pub path: PathBuf,
    pub files: u64,
    pub bytes: u64,
}

/// Результат одного пункта: итоги и разбивка по папкам, самые большие — первыми.
#[derive(Clone, Debug, Default)]
pub struct TaskResult {
    pub report: TaskReport,
    pub locations: Vec<Location>,
    /// Пояснение, если для пункта нельзя посчитать объём (например, очистка памяти при анализе).
    pub note: Option<String>,
}

/// Итоги выполнения одного пункта (или всей очистки целиком).
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskReport {
    /// Сколько файлов удалено.
    pub deleted_files: u64,
    /// Сколько места на диске освобождено, в байтах.
    pub freed_bytes: u64,
    /// Сколько файлов удалить не удалось (заняты другой программой, нет доступа ...).
    pub failed: u64,
    /// Сколько оперативной памяти освобождено, в байтах (только для очистки памяти).
    pub freed_memory: u64,
}

impl AddAssign for TaskReport {
    fn add_assign(&mut self, other: Self) {
        self.deleted_files += other.deleted_files;
        self.freed_bytes += other.freed_bytes;
        self.failed += other.failed;
        self.freed_memory += other.freed_memory;
    }
}

impl TaskReport {
    /// Краткое описание для интерфейса и журнала: «файлов: 128 · 45.3 МБ».
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.deleted_files > 0 || self.freed_bytes > 0 {
            parts.push(t!("report.files", count = self.deleted_files));
            parts.push(format_size(self.freed_bytes));
        }
        if self.freed_memory > 0 {
            parts.push(t!("report.memory", size = format_size(self.freed_memory)));
        }
        if self.failed > 0 {
            parts.push(t!("report.failed", count = self.failed));
        }
        if parts.is_empty() {
            return t!("report.nothing");
        }
        parts.join(" · ")
    }
}

/// Переводит размер в байтах в удобный для чтения вид: «1.5 ГБ».
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["unit.bytes", "unit.kilobytes", "unit.megabytes", "unit.gigabytes", "unit.terabytes"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", t!(UNITS[0]))
    } else {
        format!("{value:.1} {}", t!(UNITS[unit]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(0), "0 Б");
        assert_eq!(format_size(1023), "1023 Б");
        assert_eq!(format_size(1536), "1.5 КБ");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5.0 ГБ");
    }
}
