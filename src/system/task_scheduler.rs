//! Планировщик заданий Windows (COM-интерфейс `ITaskService`).
//! Задания HexCleaner лежат в отдельной папке `\HexCleaner`.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, VARIANT_BOOL};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::TaskScheduler::{
    IRegisteredTask, ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_ENUM_HIDDEN, TASK_LOGON_INTERACTIVE_TOKEN,
    TASK_STATE_RUNNING, TaskScheduler,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, Error, HRESULT, Result};

/// Папка заданий HexCleaner в Планировщике.
const FOLDER: &str = "HexCleaner";

/// Задание ни разу не запускалось (`SCHED_S_TASK_HAS_NOT_RUN`).
const TASK_HAS_NOT_RUN: i32 = 0x0004_1303;

/// Состояние задания в Планировщике.
#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub name: String,
    pub enabled: bool,
    pub running: bool,
    pub next_run: Option<NaiveDateTime>,
    pub last_run: Option<NaiveDateTime>,
    /// Код возврата последнего запуска (`None`, если задание ещё не запускалось).
    pub last_result: Option<i32>,
}

/// Подключение к Планировщику заданий текущего пользователя.
pub struct Scheduler {
    root: ITaskFolder,
}

impl Scheduler {
    pub fn connect() -> Result<Self> {
        unsafe {
            // Поток окна уже может быть инициализирован для COM (это делает winit) — тогда
            // повторная инициализация вернёт «уже сделано» или «другой режим», и это нормально.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
            let empty = VARIANT::default();
            service.Connect(&empty, &empty, &empty, &empty)?;
            let root = service.GetFolder(&BSTR::from("\\"))?;
            Ok(Self { root })
        }
    }

    /// Создаёт задание или заменяет существующее с тем же названием.
    pub fn register(&self, name: &str, xml: &str) -> Result<()> {
        let folder = self.folder_or_create()?;
        let empty = VARIANT::default();
        unsafe {
            folder.RegisterTask(
                &BSTR::from(name),
                &BSTR::from(xml),
                TASK_CREATE_OR_UPDATE.0,
                &empty,
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty,
            )?;
        }
        Ok(())
    }

    /// Удаляет задание. Если его уже нет — не ошибка.
    pub fn delete(&self, name: &str) -> Result<()> {
        let Some(folder) = self.folder()? else { return Ok(()) };
        match unsafe { folder.DeleteTask(&BSTR::from(name), 0) } {
            Err(error) if is_not_found(&error) => Ok(()),
            result => result,
        }
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        unsafe { self.task(name)?.SetEnabled(VARIANT_BOOL::from(enabled)) }
    }

    /// Запускает задание прямо сейчас, не дожидаясь расписания.
    pub fn run_now(&self, name: &str) -> Result<()> {
        unsafe { self.task(name)?.Run(&VARIANT::default()).map(|_| ()) }
    }

    /// Все задания из папки HexCleaner. Если папки нет — пустой список.
    pub fn tasks(&self) -> Result<Vec<TaskInfo>> {
        let Some(folder) = self.folder()? else { return Ok(Vec::new()) };
        unsafe {
            let collection = folder.GetTasks(TASK_ENUM_HIDDEN.0)?;
            let mut tasks = Vec::new();
            // Элементы коллекции нумеруются с единицы.
            for index in 1..=collection.Count()? {
                tasks.push(task_info(&collection.get_Item(&VARIANT::from(index))?)?);
            }
            Ok(tasks)
        }
    }

    fn task(&self, name: &str) -> Result<IRegisteredTask> {
        let folder = self.folder()?.ok_or_else(|| Error::from(HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)))?;
        unsafe { folder.GetTask(&BSTR::from(name)) }
    }

    /// Папка HexCleaner, если она есть.
    fn folder(&self) -> Result<Option<ITaskFolder>> {
        match unsafe { self.root.GetFolder(&BSTR::from(FOLDER)) } {
            Ok(folder) => Ok(Some(folder)),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn folder_or_create(&self) -> Result<ITaskFolder> {
        match self.folder()? {
            Some(folder) => Ok(folder),
            None => unsafe { self.root.CreateFolder(&BSTR::from(FOLDER), &VARIANT::default()) },
        }
    }
}

fn task_info(task: &IRegisteredTask) -> Result<TaskInfo> {
    unsafe {
        let last_result = task.LastTaskResult()?;
        let last_run = from_ole_date(task.LastRunTime()?);
        Ok(TaskInfo {
            name: task.Name()?.to_string(),
            enabled: task.Enabled()?.as_bool(),
            running: task.State()? == TASK_STATE_RUNNING,
            next_run: from_ole_date(task.NextRunTime()?),
            last_result: (last_run.is_some() && last_result != TASK_HAS_NOT_RUN).then_some(last_result),
            last_run,
        })
    }
}

fn is_not_found(error: &Error) -> bool {
    error.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
}

/// Дата OLE (дни с 30.12.1899, местное время) → дата и время. Ноль — «никогда».
fn from_ole_date(days: f64) -> Option<NaiveDateTime> {
    if days <= 0.0 {
        return None;
    }
    let epoch = NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
    epoch.checked_add_signed(Duration::milliseconds((days * 86_400_000.0).round() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_ole_dates() {
        assert_eq!(from_ole_date(0.0), None);
        let date = from_ole_date(46_314.125).unwrap();
        assert_eq!(date.format("%Y-%m-%d %H:%M").to_string(), "2026-10-19 03:00");
    }
}
