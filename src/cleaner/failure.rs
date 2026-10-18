//! Понятное объяснение, почему файл не удалось удалить, — для журнала очистки.

use std::path::Path;

use crate::system::delete::{self, DeleteError, DeleteFailure, LockingProcess};

/// Сколько раз за один пункт можно спросить Restart Manager, какой процесс держит файл.
/// Запрос медленный (десятки миллисекунд), а в `%TEMP%` бывают тысячи занятых файлов.
pub const LOCK_QUERIES_PER_TASK: u32 = 200;

/// Причина ошибки удаления по-русски, например «занят процессом chrome.exe (PID 1234)».
///
/// `lock_queries_left` — сколько ещё раз можно узнавать процессы-блокировщики;
/// когда лимит исчерпан, причина пишется без списка процессов.
pub fn explain(path: &Path, error: &DeleteError, lock_queries_left: &mut u32) -> String {
    match error.failure() {
        DeleteFailure::InUse => match locking_processes(path, lock_queries_left) {
            Some(processes) => format!("занят {processes}"),
            None => "занят другой программой".to_owned(),
        },
        DeleteFailure::CannotDelete => match locking_processes(path, lock_queries_left) {
            Some(processes) => format!("запущен или загружен как библиотека {processes}"),
            None => "запущен как программа или загружен как библиотека".to_owned(),
        },
        DeleteFailure::DeletePending => {
            "уже удаляется: исчезнет, когда его закроет программа, которая его держит".to_owned()
        }
        DeleteFailure::AccessDenied => match delete::file_owner(path) {
            Some(owner) => format!("нет прав, владелец {owner}"),
            None => "нет прав на удаление".to_owned(),
        },
        DeleteFailure::Other => error.to_string(),
    }
}

/// «процессом chrome.exe (PID 1234)» или «процессами a.exe (PID 1), b.exe (PID 2)».
/// `None`, если лимит запросов исчерпан или процессы найти не удалось.
fn locking_processes(path: &Path, queries_left: &mut u32) -> Option<String> {
    if *queries_left == 0 {
        return None;
    }
    *queries_left -= 1;

    let processes = delete::locking_processes(path).ok()?;
    if processes.is_empty() {
        return None;
    }
    let word = if processes.len() == 1 { "процессом" } else { "процессами" };
    let list: Vec<String> = processes.iter().map(describe_process).collect();
    Some(format!("{word} {}", list.join(", ")))
}

/// «chrome.exe (PID 1234)» или «svchost.exe (PID 980, служба DoSvc)».
fn describe_process(process: &LockingProcess) -> String {
    match &process.service {
        Some(service) => format!("{} (PID {}, служба {service})", process.name, process.pid),
        None => format!("{} (PID {})", process.name, process.pid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x1;

    #[test]
    fn explains_busy_file_with_its_process() {
        let path = std::env::temp_dir().join(format!("hexcleaner_explain_{}.tmp", std::process::id()));
        fs::write(&path, "busy").unwrap();
        let file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path).unwrap();
        let error = delete::delete_file(&path).unwrap_err();

        let mut queries_left = 1;
        let reason = explain(&path, &error, &mut queries_left);
        assert!(reason.starts_with("занят процессом"), "{reason}");
        assert!(reason.contains(&format!("(PID {})", std::process::id())), "{reason}");
        assert_eq!(queries_left, 0);

        // Лимит исчерпан — процессы больше не ищем, но причину всё равно называем.
        assert_eq!(explain(&path, &error, &mut queries_left), "занят другой программой");

        drop(file);
        fs::remove_file(&path).unwrap();
    }
}
