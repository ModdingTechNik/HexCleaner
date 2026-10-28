//! Удаление файла или папки по выбору пользователя (вкладка «Место на диске»):
//! в корзину HexCleaner или навсегда.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Stored, TrashDecision, TrashSession, TrashSettings};
use crate::cleaner::is_link;
use crate::system::delete;

/// Под каким «пунктом» удалённое вручную попадает в корзину.
pub const MANUAL_TASK_ID: &str = "disk_space";

/// Итог удаления.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoveReport {
    pub files: u64,
    pub bytes: u64,
    /// Имя очистки в корзине, если файлы перенесены в неё.
    pub trash_run: Option<String>,
    /// Файлы, которые не удалось удалить, и причины.
    pub failed: Vec<(PathBuf, String)>,
}

/// Удаляет файл или папку `path` со всем содержимым. С `trash` — в корзину: `decision` говорит,
/// что делать, если не хватит лимита. Без `trash` — навсегда. Ссылки и точки соединения
/// не открываются: они остаются на месте.
pub fn remove_path(
    path: &Path,
    title: &str,
    trash: Option<&TrashSettings>,
    decision: TrashDecision,
) -> RemoveReport {
    let mut report = RemoveReport::default();
    let mut session = trash.and_then(TrashSession::new);
    let mut remove = |file: &Path, size: u64, report: &mut RemoveReport| {
        clear_readonly(file);
        let stored = match session.as_mut() {
            Some(session) => session.store(MANUAL_TASK_ID, title, file, size, &mut |_| decision),
            None => Stored::NotMoved,
        };
        let result = match stored {
            Stored::Moved => Ok(()),
            Stored::NotMoved => delete::delete_file(file).map_err(|error| error.to_string()),
            Stored::Failed(error) => Err(error.to_string()),
            Stored::Stopped => return,
        };
        match result {
            Ok(()) => {
                report.files += 1;
                report.bytes += size;
            }
            Err(error) => report.failed.push((file.to_owned(), error)),
        }
    };

    match fs::symlink_metadata(path) {
        Ok(metadata) if is_link(&metadata) => {}
        Ok(metadata) if metadata.is_dir() => {
            remove_dir(path, &mut report, &mut remove);
        }
        Ok(metadata) => remove(path, metadata.len(), &mut report),
        Err(error) => report.failed.push((path.to_owned(), error.to_string())),
    }
    report.trash_run = session.and_then(TrashSession::finish);
    report
}

/// Удаляет содержимое папки, затем саму папку, если она опустела.
fn remove_dir(dir: &Path, report: &mut RemoveReport, remove: &mut dyn FnMut(&Path, u64, &mut RemoveReport)) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            report.failed.push((dir.to_owned(), error.to_string()));
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if is_link(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            remove_dir(&entry.path(), report, remove);
        } else {
            remove(&entry.path(), metadata.len(), report);
        }
    }
    let _ = fs::remove_dir(dir);
}

fn clear_readonly(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        if permissions.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            let _ = fs::set_permissions(path, permissions);
        }
    }
}
