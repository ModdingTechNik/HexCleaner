//! Права администратора: проверка и перезапуск программы с повышением прав (UAC).

use std::mem::size_of;

use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

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
/// Возвращает ошибку, если пользователь отказался в окне UAC.
/// При успехе текущий экземпляр нужно закрыть.
pub fn restart_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;

    let result = unsafe {
        ShellExecuteW(None, w!("runas"), &HSTRING::from(exe.as_os_str()), None, None, SW_SHOWNORMAL)
    };

    // По документации ShellExecute значение больше 32 означает успех.
    if result.0 as usize > 32 {
        Ok(())
    } else {
        Err("Не удалось получить права администратора (запрос UAC отклонён).".to_owned())
    }
}
