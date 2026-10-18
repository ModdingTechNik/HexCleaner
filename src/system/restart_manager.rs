//! Restart Manager — механизм Windows, через который установщики узнают, какие программы
//! держат файлы, корректно закрывают их и потом запускают снова.

use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_MORE_DATA, FILETIME};
use windows::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_APP_TYPE, RM_PROCESS_INFO, RM_UNIQUE_PROCESS, RmConsole, RmCritical, RmEndSession,
    RmExplorer, RmGetList, RmMainWindow, RmOtherWindow, RmRegisterResources, RmRestart, RmService, RmShutdown,
    RmStartSession,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use super::OwnedHandle;

/// Процесс. Номер процесса Windows переиспользует, поэтому вместе с ним хранится время запуска.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessId {
    pub pid: u32,
    start_time: u64,
}

impl ProcessId {
    fn to_raw(self) -> RM_UNIQUE_PROCESS {
        RM_UNIQUE_PROCESS {
            dwProcessId: self.pid,
            ProcessStartTime: FILETIME {
                dwLowDateTime: self.start_time as u32,
                dwHighDateTime: (self.start_time >> 32) as u32,
            },
        }
    }

    fn from_raw(process: &RM_UNIQUE_PROCESS) -> Self {
        let time = &process.ProcessStartTime;
        Self { pid: process.dwProcessId, start_time: (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime) }
    }
}

/// Что это за программа с точки зрения Restart Manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKind {
    /// Обычная программа с окном.
    Window,
    Console,
    Service,
    /// Проводник Windows (рабочий стол и панель задач).
    Explorer,
    /// Критический системный процесс — закрывать нельзя.
    Critical,
    Unknown,
}

impl AppKind {
    // Константы названы как в Windows SDK (`RmMainWindow`), а не по правилам Rust.
    #[allow(non_upper_case_globals)]
    fn from_raw(kind: RM_APP_TYPE) -> Self {
        match kind {
            RmMainWindow | RmOtherWindow => AppKind::Window,
            RmConsole => AppKind::Console,
            RmService => AppKind::Service,
            RmExplorer => AppKind::Explorer,
            RmCritical => AppKind::Critical,
            _ => AppKind::Unknown,
        }
    }
}

/// Программа, которая держит зарегистрированные файлы.
#[derive(Clone, Debug)]
pub struct AffectedApp {
    pub process: ProcessId,
    /// Имя исполняемого файла, например `chrome.exe`.
    pub exe: String,
    /// Название программы, например «Google Chrome».
    pub name: String,
    /// Короткое имя службы, если файлы держит служба Windows.
    pub service: Option<String>,
    pub kind: AppKind,
    /// Умеет ли программа запуститься снова после закрытия (браузеры при этом восстанавливают вкладки).
    pub restartable: bool,
}

/// Сеанс Restart Manager. Закрывается автоматически.
pub struct Session(u32);

impl Session {
    pub fn start() -> io::Result<Self> {
        let mut handle = 0;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        unsafe { RmStartSession(&mut handle, None, PWSTR(key.as_mut_ptr())) }.ok()?;
        Ok(Self(handle))
    }

    /// Файлы, для которых нужно найти (или закрыть) программы, которые их держат.
    pub fn register_files(&self, files: &[PathBuf]) -> io::Result<()> {
        let names: Vec<HSTRING> = files.iter().map(|file| HSTRING::from(file.as_os_str())).collect();
        let pointers: Vec<PCWSTR> = names.iter().map(|name| PCWSTR(name.as_ptr())).collect();
        unsafe { RmRegisterResources(self.0, Some(&pointers), None, None) }.ok()?;
        Ok(())
    }

    /// Процессы, которые нужно закрыть (и потом, возможно, запустить снова).
    pub fn register_processes(&self, processes: &[ProcessId]) -> io::Result<()> {
        let raw: Vec<RM_UNIQUE_PROCESS> = processes.iter().map(|process| process.to_raw()).collect();
        unsafe { RmRegisterResources(self.0, None, Some(&raw), None) }.ok()?;
        Ok(())
    }

    /// Программы, затронутые зарегистрированными файлами и процессами.
    pub fn affected_apps(&self) -> io::Result<Vec<AffectedApp>> {
        // Сначала узнаём количество, затем получаем список.
        // Между вызовами список может вырасти, поэтому повторяем, пока не поместится.
        let mut infos: Vec<RM_PROCESS_INFO> = Vec::new();
        loop {
            let mut needed = 0;
            let mut count = infos.len() as u32;
            let mut reboot_reasons = 0;
            let buffer = (!infos.is_empty()).then_some(infos.as_mut_ptr());
            let result = unsafe { RmGetList(self.0, &mut needed, &mut count, buffer, &mut reboot_reasons) };

            if result == ERROR_MORE_DATA {
                infos.resize(needed as usize, RM_PROCESS_INFO::default());
                continue;
            }
            result.ok()?;
            infos.truncate(count as usize);
            break;
        }
        Ok(infos.iter().map(affected_app).collect())
    }

    /// Корректно закрывает затронутые программы — так же, как при выходе из Windows:
    /// программы успевают сохранить данные. Принудительно ничего не завершается,
    /// поэтому программа, которая отказалась закрыться, останется работать.
    pub fn shutdown(&self) -> io::Result<()> {
        unsafe { RmShutdown(self.0, 0, None) }.ok()?;
        Ok(())
    }

    /// Запускает снова программы, закрытые [`Session::shutdown`] и умеющие перезапускаться.
    pub fn restart(&self) -> io::Result<()> {
        unsafe { RmRestart(self.0, None, None) }.ok()?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = RmEndSession(self.0);
        }
    }
}

/// Сколько ждать, пока закрываемые программы завершат все свои процессы.
const CLOSE_WAIT: Duration = Duration::from_secs(10);

/// Программы, закрытые перед очисткой.
pub struct ClosedPrograms {
    /// Сеанс, в котором программы закрывались: через него же их можно запустить снова.
    session: Session,
    /// Программы, которые отказались закрыться (например, спросили о несохранённых данных).
    pub still_running: Vec<String>,
}

impl ClosedPrograms {
    /// Запускает закрытые программы снова (те, что это поддерживают).
    pub fn restart(&self) -> io::Result<()> {
        self.session.restart()
    }
}

/// Корректно закрывает программы целиком — все процессы с этими именами файлов.
///
/// У браузера много процессов, и файлы кэша часто держит служебный процесс без окна,
/// поэтому закрываем программу целиком: главное окно получает запрос на завершение,
/// как при выходе из Windows, а служебные процессы завершаются вместе с ним.
pub fn close_programs(exes: &[String]) -> io::Result<ClosedPrograms> {
    let processes: Vec<ProcessId> = exes.iter().flat_map(|exe| processes_of(exe)).collect();
    let session = Session::start()?;
    if !processes.is_empty() {
        session.register_processes(&processes)?;
        // Ошибку не проверяем: Restart Manager жалуется и на служебные процессы, которые
        // закрылись сами вслед за главным окном. Что закрылось на самом деле — проверяем ниже.
        let _ = super::console::preserve_std_handles(|| session.shutdown());
    }

    let started = Instant::now();
    let still_running = loop {
        let running: Vec<String> = exes.iter().filter(|exe| !processes_of(exe).is_empty()).cloned().collect();
        if running.is_empty() || started.elapsed() >= CLOSE_WAIT {
            break running;
        }
        thread::sleep(Duration::from_millis(250));
    };
    Ok(ClosedPrograms { session, still_running })
}

/// Все запущенные процессы с таким именем файла, например все `chrome.exe`.
pub fn processes_of(exe: &str) -> Vec<ProcessId> {
    super::memory::process_ids()
        .into_iter()
        .filter(|&pid| process_exe_name(pid).is_some_and(|name| name.eq_ignore_ascii_case(exe)))
        .filter_map(process_id)
        .collect()
}

/// Номер процесса вместе со временем запуска.
fn process_id(pid: u32) -> Option<ProcessId> {
    let process = OwnedHandle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    Some(ProcessId::from_raw(&RM_UNIQUE_PROCESS { dwProcessId: pid, ProcessStartTime: created }))
}

/// Программы, которые держат файл открытым или загрузили его как библиотеку.
pub fn apps_using(path: &Path) -> io::Result<Vec<AffectedApp>> {
    let session = Session::start()?;
    session.register_files(&[path.to_owned()])?;
    session.affected_apps()
}

fn affected_app(info: &RM_PROCESS_INFO) -> AffectedApp {
    let process = ProcessId::from_raw(&info.Process);
    let name = from_wide(&info.strAppName);
    let service = from_wide(&info.strServiceShortName);
    AffectedApp {
        process,
        exe: process_exe_name(process.pid).unwrap_or_else(|| name.clone()),
        name,
        service: (!service.is_empty()).then_some(service),
        kind: AppKind::from_raw(info.ApplicationType),
        restartable: info.bRestartable.as_bool(),
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

/// Строка из буфера UTF-16 с завершающим нулём.
fn from_wide(buffer: &[u16]) -> String {
    let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// Копия Блокнота под уникальным именем: закрываться должна только она,
    /// а не Блокноты, открытые пользователем.
    #[test]
    fn closes_program_by_exe_name() {
        let windir = PathBuf::from(std::env::var("WINDIR").unwrap());
        let dir = std::env::temp_dir().join(format!("hexcleaner_close_{}", std::process::id()));
        let exe = format!("hexcleaner_test_{}.exe", std::process::id());
        fs::create_dir_all(&dir).unwrap();
        fs::copy(windir.join(r"System32\notepad.exe"), dir.join(&exe)).unwrap();
        // Без файла ресурсов (.mui) рядом копия Блокнота не запускается.
        for entry in fs::read_dir(windir.join("System32")).unwrap().filter_map(Result::ok) {
            let mui = entry.path().join("notepad.exe.mui");
            if mui.is_file() {
                fs::create_dir_all(dir.join(entry.file_name())).unwrap();
                fs::copy(&mui, dir.join(entry.file_name()).join(format!("{exe}.mui"))).unwrap();
            }
        }

        let mut notepad = Command::new(dir.join(&exe)).spawn().unwrap();
        thread::sleep(Duration::from_secs(2));
        assert_eq!(processes_of(&exe).len(), 1, "тестовый Блокнот не запустился");

        let closed = close_programs(std::slice::from_ref(&exe)).unwrap();
        assert!(closed.still_running.is_empty());
        assert!(processes_of(&exe).is_empty());

        let _ = notepad.kill();
        let _ = notepad.wait();
        let _ = fs::remove_dir_all(&dir);
    }
}
