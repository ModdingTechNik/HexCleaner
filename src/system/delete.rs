//! Удаление файла с выяснением причины, если удалить не получилось.
//!
//! Код ошибки Windows 5 «Отказано в доступе» объединяет несколько разных ситуаций.
//! Чтобы их различить, сразу после неудачного `DeleteFileW` читаем исходный
//! NTSTATUS ядра (`RtlGetLastNtStatus`) — он точнее. Дополнительно можно узнать,
//! какие процессы держат файл (Restart Manager), и кто владелец файла.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_MORE_DATA, ERROR_SHARING_VIOLATION, HLOCAL, LocalFree, NTSTATUS};
use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows::Win32::Security::{
    LookupAccountSidW, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SID_NAME_USE,
};
use windows::Win32::Storage::FileSystem::DeleteFileW;
use windows::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources, RmStartSession,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use super::OwnedHandle;

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

/// Процесс, который держит файл открытым.
#[derive(Debug)]
pub struct LockingProcess {
    pub pid: u32,
    /// Имя исполняемого файла (например, `chrome.exe`) или название приложения.
    pub name: String,
    /// Короткое имя службы, если файл держит служба Windows.
    pub service: Option<String>,
}

/// Процессы, которые держат файл открытым или загрузили его как библиотеку.
///
/// Работает через Restart Manager — тот же механизм, что и окно «Файл уже используется»
/// в проводнике. Вызов медленный (десятки миллисекунд), поэтому не стоит делать его для каждого файла.
pub fn locking_processes(path: &Path) -> io::Result<Vec<LockingProcess>> {
    let session = RestartManagerSession::start()?;

    let file = HSTRING::from(path.as_os_str());
    let files = [PCWSTR(file.as_ptr())];
    unsafe { RmRegisterResources(session.0, Some(&files), None, None) }.ok()?;

    // Сначала узнаём количество процессов, затем получаем список.
    // Между вызовами список может вырасти, поэтому повторяем, пока не поместится.
    let mut infos: Vec<RM_PROCESS_INFO> = Vec::new();
    loop {
        let mut needed = 0;
        let mut count = infos.len() as u32;
        let mut reboot_reasons = 0;
        let buffer = (!infos.is_empty()).then_some(infos.as_mut_ptr());
        let result = unsafe { RmGetList(session.0, &mut needed, &mut count, buffer, &mut reboot_reasons) };

        if result == ERROR_MORE_DATA {
            infos.resize(needed as usize, RM_PROCESS_INFO::default());
            continue;
        }
        result.ok()?;
        infos.truncate(count as usize);
        break;
    }

    Ok(infos.iter().map(locking_process).collect())
}

fn locking_process(info: &RM_PROCESS_INFO) -> LockingProcess {
    let pid = info.Process.dwProcessId;
    let service = from_wide(&info.strServiceShortName);
    LockingProcess {
        pid,
        name: process_exe_name(pid).unwrap_or_else(|| from_wide(&info.strAppName)),
        service: (!service.is_empty()).then_some(service),
    }
}

/// Имя исполняемого файла процесса, например `chrome.exe`.
fn process_exe_name(pid: u32) -> Option<String> {
    let process = OwnedHandle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length) }
        .ok()?;
    let full_path = PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize]));
    Some(full_path.file_name()?.to_string_lossy().into_owned())
}

/// Сеанс Restart Manager, который автоматически закрывается.
struct RestartManagerSession(u32);

impl RestartManagerSession {
    fn start() -> io::Result<Self> {
        let mut handle = 0;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        unsafe { RmStartSession(&mut handle, None, PWSTR(key.as_mut_ptr())) }.ok()?;
        Ok(Self(handle))
    }
}

impl Drop for RestartManagerSession {
    fn drop(&mut self) {
        unsafe {
            let _ = RmEndSession(self.0);
        }
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

        let lockers = locking_processes(&path).unwrap();
        assert!(
            lockers.iter().any(|process| process.pid == std::process::id()),
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
