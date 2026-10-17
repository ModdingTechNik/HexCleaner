//! Остановка и запуск служб Windows (нужно, чтобы освободить файлы кэша обновлений).

use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_HANDLE,
    SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START,
    SERVICE_STATUS, SERVICE_STOP, SERVICE_STOPPED, StartServiceW,
};
use windows::core::{HSTRING, Result};

/// Сколько ждать, пока служба остановится.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Останавливает службу и ждёт её остановки.
/// Возвращает `true`, если служба работала и была остановлена (её нужно будет запустить обратно).
pub fn stop(name: &str) -> Result<bool> {
    let service = Service::open(name)?;
    if service.status()?.dwCurrentState != SERVICE_RUNNING {
        return Ok(false);
    }

    let mut status = SERVICE_STATUS::default();
    unsafe { ControlService(service.handle, SERVICE_CONTROL_STOP, &mut status)? };

    let started = Instant::now();
    while service.status()?.dwCurrentState != SERVICE_STOPPED && started.elapsed() < STOP_TIMEOUT {
        thread::sleep(Duration::from_millis(250));
    }
    Ok(true)
}

/// Запускает службу.
pub fn start(name: &str) -> Result<()> {
    let service = Service::open(name)?;
    unsafe { StartServiceW(service.handle, None) }
}

/// Открытая служба вместе с диспетчером служб; оба дескриптора закрываются автоматически.
struct Service {
    manager: SC_HANDLE,
    handle: SC_HANDLE,
}

impl Service {
    fn open(name: &str) -> Result<Self> {
        unsafe {
            let manager = OpenSCManagerW(None, None, SC_MANAGER_CONNECT)?;
            let access = SERVICE_STOP | SERVICE_START | SERVICE_QUERY_STATUS;
            match OpenServiceW(manager, &HSTRING::from(name), access) {
                Ok(handle) => Ok(Self { manager, handle }),
                Err(error) => {
                    let _ = CloseServiceHandle(manager);
                    Err(error)
                }
            }
        }
    }

    fn status(&self) -> Result<SERVICE_STATUS> {
        let mut status = SERVICE_STATUS::default();
        unsafe { QueryServiceStatus(self.handle, &mut status)? };
        Ok(status)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.handle);
            let _ = CloseServiceHandle(self.manager);
        }
    }
}
