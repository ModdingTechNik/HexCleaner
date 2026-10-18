//! Консоль: перехват Ctrl+C, чтобы командная строка остановила очистку аккуратно —
//! с запуском остановленных служб и записью журнала, а не обрывала процесс на полпути,
//! и защита стандартных потоков от подмены при закрытии программ.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{FALSE, TRUE};
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, CTRL_BREAK_EVENT, CTRL_C_EVENT, GetConsoleWindow, GetStdHandle,
    STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetStdHandle,
};
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

/// Выполняет `action` и затем возвращает процессу его консоль и стандартные потоки.
///
/// Restart Manager, закрывая консольную программу, подключается к её консоли и подменяет
/// стандартные потоки нашего процесса. Без этой защиты весь дальнейший вывод
/// командной строки уходил бы в консоль уже закрытой программы.
pub fn preserve_std_handles<T>(action: impl FnOnce() -> T) -> T {
    const STREAMS: [STD_HANDLE; 3] = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
    let had_console = unsafe { !GetConsoleWindow().is_invalid() };
    let handles = STREAMS.map(|stream| unsafe { GetStdHandle(stream) }.unwrap_or_default());

    let result = action();

    unsafe {
        if had_console && GetConsoleWindow().is_invalid() {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
        for (stream, handle) in STREAMS.into_iter().zip(handles) {
            let _ = SetStdHandle(stream, handle);
        }
    }
    result
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
