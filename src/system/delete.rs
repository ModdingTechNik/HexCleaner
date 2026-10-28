//! Удаление файла с выяснением причины, если удалить не получилось.
//!
//! Код ошибки Windows 5 «Отказано в доступе» объединяет несколько разных ситуаций.
//! Чтобы их различить, сразу после неудачного `DeleteFileW` читаем исходный
//! NTSTATUS ядра (`RtlGetLastNtStatus`) — он точнее. Дополнительно можно узнать
//! владельца файла. Какие процессы держат файл — см. `restart_manager::apps_using`.

use std::fmt;
use std::io;
use std::path::Path;

use windows::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, HLOCAL, LocalFree, NTSTATUS};
use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows::Win32::Security::{
    LookupAccountSidW, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SID_NAME_USE,
};
use windows::Win32::Storage::FileSystem::{DeleteFileW, MOVEFILE_COPY_ALLOWED, MoveFileExW};
use windows::core::{HSTRING, PCWSTR, PWSTR};

const STATUS_ACCESS_DENIED: NTSTATUS = NTSTATUS(0xC000_0022_u32 as i32);
const STATUS_SHARING_VIOLATION: NTSTATUS = NTSTATUS(0xC000_0043_u32 as i32);
const STATUS_DELETE_PENDING: NTSTATUS = NTSTATUS(0xC000_0056_u32 as i32);
const STATUS_CANNOT_DELETE: NTSTATUS = NTSTATUS(0xC000_0121_u32 as i32);

/// Почему файл не удалось удалить.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteFailure {
    /// Файл открыт другой программой без разрешения на удаление.
    InUse,
    /// Файл запущен как программа или загружен как библиотека (либо помечен «только чтение»).
    CannotDelete,
    /// Файл уже помечен на удаление и исчезнет, когда его закроют.
    DeletePending,
    /// Нет прав на удаление по списку доступа (ACL).
    AccessDenied,
    /// Другая ошибка — см. текст ошибки Windows.
    Other,
}

/// Ошибка удаления: код Windows и исходный NTSTATUS.
#[derive(Debug)]
pub struct DeleteError {
    error: io::Error,
    status: NTSTATUS,
}

impl DeleteError {
    pub fn failure(&self) -> DeleteFailure {
        classify(self.status, &self.error)
    }
}

impl fmt::Display for DeleteError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.error.fmt(f)
    }
}

/// Удаляет файл. При ошибке запоминает NTSTATUS, пока его не затёрли другие вызовы.
pub fn delete_file(path: &Path) -> Result<(), DeleteError> {
    windows::core::link!("ntdll.dll" "system" fn RtlGetLastNtStatus() -> NTSTATUS);

    let result = unsafe { DeleteFileW(&HSTRING::from(path.as_os_str())) };
    result.map_err(|error| DeleteError {
        status: unsafe { RtlGetLastNtStatus() },
        error: io::Error::from_raw_os_error(error.code().0 & 0xFFFF),
    })
}

/// Переносит файл (в корзину или из неё). На другой диск Windows переносит копированием
/// с удалением оригинала. Ошибка — та же, что у удаления: файл занят, нет прав и т. п.
pub fn move_file(from: &Path, to: &Path) -> Result<(), DeleteError> {
    windows::core::link!("ntdll.dll" "system" fn RtlGetLastNtStatus() -> NTSTATUS);

    let result =
        unsafe { MoveFileExW(&HSTRING::from(from.as_os_str()), &HSTRING::from(to.as_os_str()), MOVEFILE_COPY_ALLOWED) };
    result.map_err(|error| DeleteError {
        status: unsafe { RtlGetLastNtStatus() },
        error: io::Error::from_raw_os_error(error.code().0 & 0xFFFF),
    })
}

fn classify(status: NTSTATUS, error: &io::Error) -> DeleteFailure {
    match status {
        STATUS_SHARING_VIOLATION => DeleteFailure::InUse,
        STATUS_CANNOT_DELETE => DeleteFailure::CannotDelete,
        STATUS_DELETE_PENDING => DeleteFailure::DeletePending,
        STATUS_ACCESS_DENIED => DeleteFailure::AccessDenied,
        // NTSTATUS неизвестен — пробуем понять по обычному коду ошибки Windows.
        _ => match error.raw_os_error().map(|code| code as u32) {
            Some(code) if code == ERROR_SHARING_VIOLATION.0 || code == ERROR_LOCK_VIOLATION.0 => DeleteFailure::InUse,
            _ => DeleteFailure::Other,
        },
    }
}

/// Владелец файла в виде `ДОМЕН\Имя`, например `NT SERVICE\TrustedInstaller`.
pub fn file_owner(path: &Path) -> Option<String> {
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let result = unsafe {
        GetNamedSecurityInfoW(
            &HSTRING::from(path.as_os_str()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            &mut descriptor,
        )
    };
    if result.is_err() {
        return None;
    }

    // `owner` указывает внутрь `descriptor`, поэтому освобождаем его только после использования.
    let name = account_name(owner);
    unsafe {
        LocalFree(Some(HLOCAL(descriptor.0)));
    }
    name
}

fn account_name(sid: PSID) -> Option<String> {
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let mut name_length = name.len() as u32;
    let mut domain_length = domain.len() as u32;
    let mut sid_type = SID_NAME_USE::default();
    unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_length,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_length,
            &mut sid_type,
        )
    }
    .ok()?;

    let name = from_wide(&name);
    let domain = from_wide(&domain);
    Some(if domain.is_empty() { name } else { format!(r"{domain}\{name}") })
}

/// Строка из буфера UTF-16 с завершающим нулём.
fn from_wide(buffer: &[u16]) -> String {
    let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x1;

    #[test]
    fn busy_file_reports_its_locking_process() {
        let path = std::env::temp_dir().join(format!("hexcleaner_locked_{}.tmp", std::process::id()));
        fs::write(&path, "busy").unwrap();

        // Открываем файл, не разрешая другим его удалять, — как это делают многие программы.
        let file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path).unwrap();

        let error = delete_file(&path).expect_err("занятый файл не должен удаляться");
        assert_eq!(error.failure(), DeleteFailure::InUse);

        let lockers = crate::system::restart_manager::apps_using(&path).unwrap();
        assert!(
            lockers.iter().any(|app| app.process.pid == std::process::id()),
            "в списке нет текущего процесса: {lockers:?}"
        );

        drop(file);
        delete_file(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn system_files_are_owned_by_trusted_installer() {
        let notepad = Path::new(&std::env::var("WINDIR").unwrap()).join(r"System32\notepad.exe");
        assert_eq!(file_owner(&notepad).as_deref(), Some(r"NT SERVICE\TrustedInstaller"));
    }

    #[test]
    fn classifies_nt_status() {
        let access_denied = io::Error::from_raw_os_error(5);
        assert_eq!(classify(STATUS_CANNOT_DELETE, &access_denied), DeleteFailure::CannotDelete);
        assert_eq!(classify(STATUS_DELETE_PENDING, &access_denied), DeleteFailure::DeletePending);
        assert_eq!(classify(STATUS_ACCESS_DENIED, &access_denied), DeleteFailure::AccessDenied);
        assert_eq!(classify(NTSTATUS(0), &io::Error::from_raw_os_error(32)), DeleteFailure::InUse);
        assert_eq!(classify(NTSTATUS(0), &access_denied), DeleteFailure::Other);
    }
}
