//! Консоль: перехват Ctrl+C, чтобы командная строка остановила очистку аккуратно —
//! с запуском остановленных служб и записью журнала, а не обрывала процесс на полпути.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{FALSE, TRUE};
use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows::core::BOOL;

static CTRL_C_PRESSED: AtomicBool = AtomicBool::new(false);

/// Перехватывает Ctrl+C и Ctrl+Break: процесс не завершается, а [`ctrl_c_pressed`] возвращает `true`.
pub fn catch_ctrl_c() {
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handle_ctrl), true);
    }
}

/// Нажимал ли пользователь Ctrl+C с момента [`catch_ctrl_c`].
pub fn ctrl_c_pressed() -> bool {
    CTRL_C_PRESSED.load(Ordering::Relaxed)
}

unsafe extern "system" fn handle_ctrl(ctrl_type: u32) -> BOOL {
    if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
        CTRL_C_PRESSED.store(true, Ordering::Relaxed);
        TRUE
    } else {
        // Закрытие окна консоли и выход из системы обрабатывает Windows как обычно.
        FALSE
    }
}
