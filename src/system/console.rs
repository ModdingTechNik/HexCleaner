//! Консоль: перехват Ctrl+C, чтобы командная строка остановила очистку аккуратно —
//! с запуском остановленных служб и записью журнала, а не обрывала процесс на полпути,
//! защита стандартных потоков от подмены при закрытии программ
//! и вывод оконной программы, запущенной с параметрами.

use std::fs::File;
use std::os::windows::io::IntoRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{FALSE, HANDLE, TRUE};
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, CTRL_BREAK_EVENT, CTRL_C_EVENT, FreeConsole, GetConsoleWindow, GetStdHandle,
    STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetStdHandle,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW};
use windows::core::{BOOL, HSTRING};

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

/// Направляет вывод программы в файлы и отключает её от консоли. Так работает копия программы,
/// запущенная с правами администратора по `--elevate`: её вывод показывает исходная программа.
/// Файлы остаются открытыми до конца работы программы.
pub fn redirect_output(stdout: File, stderr: File) {
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, HANDLE(stdout.into_raw_handle()));
        let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(stderr.into_raw_handle()));
        let _ = FreeConsole();
    }
}

/// Подключает оконную программу к консоли, из которой её запустили, чтобы её вывод было видно.
///
/// Возвращает `false`, если выводить некуда: программу запустили не из консоли
/// (ярлыком, через «Выполнить», Планировщиком) и вывод не перенаправлен в файл.
/// Консоль не ждёт оконную программу и уже напечатала приглашение, поэтому вывод
/// начинается с новой строки.
pub fn attach_parent_console() -> bool {
    let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if output.is_ok_and(|handle| !handle.is_invalid()) {
        return true;
    }
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS).is_ok() };
    if attached {
        println!();
    }
    attached
}

/// Показывает текст в окне сообщения — для оконной программы, которой некуда его напечатать.
pub fn message_box(title: &str, text: &str, is_error: bool) {
    let icon = if is_error { MB_ICONERROR } else { MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_OK | icon);
    }
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
