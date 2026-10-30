//! Права администратора: проверка и перезапуск программы с повышением прав (UAC).

use std::io;
use std::mem::size_of;
use std::path::Path;
use std::time::Duration;

use windows::Win32::Foundation::{ERROR_CANCELLED, WAIT_OBJECT_0};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};
use windows::core::{HSTRING, PCWSTR, w};

use super::OwnedHandle;

/// Запущена ли программа с правами администратора.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = Default::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let token = OwnedHandle(token);

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0;
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok()
            && elevation.TokenIsElevated != 0
    }
}

/// Запускает новый экземпляр программы с запросом прав администратора.
///
/// Возвращает `false`, если пользователь отказался в окне UAC.
/// При успехе текущий экземпляр нужно закрыть.
pub fn restart_as_admin() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };

    let result = unsafe {
        ShellExecuteW(None, w!("runas"), &HSTRING::from(exe.as_os_str()), None, None, SW_SHOWNORMAL)
    };

    // По документации ShellExecute значение больше 32 означает успех.
    result.0 as usize > 32
}

/// Почему не удалось запустить программу с правами администратора.
#[derive(Debug)]
pub enum ElevateError {
    /// Пользователь отказался в окне UAC.
    Cancelled,
    Failed(io::Error),
}

/// Программа, запущенная с правами администратора.
pub struct ElevatedProcess(OwnedHandle);

impl ElevatedProcess {
    /// Ждёт не дольше `timeout`. Код возврата, если программа уже завершилась.
    pub fn wait(&self, timeout: Duration) -> Option<u32> {
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        unsafe {
            if WaitForSingleObject(self.0.0, millis) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0;
            GetExitCodeProcess(self.0.0, &mut code).ok()?;
            Some(code)
        }
    }
}

/// Запускает `exe` с параметрами `parameters` и запросом прав администратора, без окна.
/// Ждёт, пока пользователь ответит в окне UAC.
pub fn start_elevated(exe: &Path, parameters: &str) -> Result<ElevatedProcess, ElevateError> {
    let exe = HSTRING::from(exe.as_os_str());
    let parameters = HSTRING::from(parameters);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|error| {
        if error.code() == ERROR_CANCELLED.to_hresult() {
            ElevateError::Cancelled
        } else {
            ElevateError::Failed(io::Error::from_raw_os_error(error.code().0 & 0xFFFF))
        }
    })?;
    if info.hProcess.is_invalid() {
        return Err(ElevateError::Failed(io::Error::other("no process handle")));
    }
    Ok(ElevatedProcess(OwnedHandle(info.hProcess)))
}
