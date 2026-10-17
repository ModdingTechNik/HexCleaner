//! Удаление файлов и папок.
//!
//! Правила безопасности:
//! * символические ссылки и точки соединения (junction) никогда не открываются
//!   и не удаляются — за ними может оказаться папка с пользовательскими файлами;
//! * занятые файлы просто пропускаются, очистка продолжается.

use std::fs::{self, Metadata};
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::TaskRun;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Удаляет всё содержимое папки `dir`, саму папку оставляет.
///
/// Если задан `min_age`, файлы и папки моложе этого возраста не трогаются.
/// Возвращает `true`, если папка стала полностью пустой.
pub fn clear_directory(run: &mut TaskRun, dir: &Path, min_age: Option<Duration>) -> bool {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            run.skipped(dir, &error.to_string());
            return false;
        }
    };

    let mut is_empty = true;
    for entry in entries {
        if run.is_cancelled() {
            return false;
        }
        let Ok(entry) = entry else {
            is_empty = false;
            continue;
        };
        let path = entry.path();
        // На Windows `DirEntry::metadata` не переходит по ссылкам.
        let Ok(metadata) = entry.metadata() else {
            is_empty = false;
            continue;
        };

        if is_link(&metadata) {
            run.skipped(&path, "ссылка на другую папку, не трогаем");
            is_empty = false;
        } else if metadata.is_dir() {
            // Возраст проверяем до очистки: после удаления файлов дата папки обновится.
            let old_enough = is_old_enough(&metadata, min_age);
            if clear_directory(run, &path, min_age) && old_enough {
                is_empty &= remove_empty_directory(run, &path, &metadata);
            } else {
                is_empty = false;
            }
        } else if is_old_enough(&metadata, min_age) {
            is_empty &= delete_file(run, &path, &metadata);
        } else {
            is_empty = false;
        }
    }
    is_empty
}

/// Удаляет папку `dir` вместе со всем содержимым.
pub fn delete_directory(run: &mut TaskRun, dir: &Path, metadata: &Metadata) {
    if clear_directory(run, dir, None) {
        remove_empty_directory(run, dir, metadata);
    }
}

/// Удаляет один файл и записывает результат в статистику и журнал.
pub fn delete_file(run: &mut TaskRun, path: &Path, metadata: &Metadata) -> bool {
    clear_readonly(path, metadata);
    match fs::remove_file(path) {
        Ok(()) => {
            run.file_deleted(path, metadata.len());
            true
        }
        Err(error) => {
            run.failed(path, &error);
            false
        }
    }
}

/// Суммарный размер всех файлов в папке (без перехода по ссылкам).
pub fn directory_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?)))
        .map(|(path, metadata)| {
            if is_link(&metadata) {
                0
            } else if metadata.is_dir() {
                directory_size(&path)
            } else {
                metadata.len()
            }
        })
        .sum()
}

/// Символическая ссылка, точка соединения или другая точка повторного анализа.
pub fn is_link(metadata: &Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn remove_empty_directory(run: &mut TaskRun, dir: &Path, metadata: &Metadata) -> bool {
    clear_readonly(dir, metadata);
    match fs::remove_dir(dir) {
        Ok(()) => true,
        Err(error) => {
            run.skipped(dir, &error.to_string());
            false
        }
    }
}

fn is_old_enough(metadata: &Metadata, min_age: Option<Duration>) -> bool {
    let Some(min_age) = min_age else {
        return true;
    };
    let age = metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok());
    // Дата в будущем или недоступна — на всякий случай считаем файл свежим.
    age.is_some_and(|age| age >= min_age)
}

/// Снимает атрибут «только чтение», иначе Windows не даст удалить файл.
fn clear_readonly(path: &Path, metadata: &Metadata) {
    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }
}
