//! Уведомления Windows: мигание кнопки окна на панели задач и всплывающее сообщение
//! в углу экрана, которое остаётся в Центре уведомлений. Сообщения пользователь может
//! отключить в настройках Windows, мигание — нет.
//!
//! Используется значок в области уведомлений (`Shell_NotifyIcon`): в отличие от toast-API,
//! он не требует регистрировать программу в системе. Значок живёт, пока жив [`Notification`].

use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_TIP, NIIF_LARGE_ICON, NIIF_USER, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetSystemMetrics, HICON, IMAGE_ICON,
    LR_DEFAULTCOLOR, LoadImageW, SM_CXICON, SM_CXSMICON,
};
use windows::core::PCWSTR;

/// Кнопка окна `window` на панели задач мигает, пока пользователь не откроет окно.
pub fn flash_taskbar(window: isize) {
    let info = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd: HWND(window as *mut _),
        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

/// Номер иконки в ресурсах exe (`assets/common.rc`).
const ICON_RESOURCE_ID: u16 = 1;

/// Номер нашего значка в области уведомлений — у окна он один.
const NOTIFY_ICON_ID: u32 = 1;

/// Показанное уведомление. При удалении значок из области уведомлений убирается,
/// а само сообщение остаётся в Центре уведомлений.
pub struct Notification {
    data: NOTIFYICONDATAW,
}

impl Notification {
    /// Показывает уведомление от имени окна `window` (дескриптор окна программы).
    pub fn show(window: isize, title: &str, text: &str) -> Option<Self> {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: HWND(window as *mut _),
            uID: NOTIFY_ICON_ID,
            uFlags: NIF_ICON | NIF_TIP | NIF_INFO,
            hIcon: load_icon(SM_CXSMICON)?,
            dwInfoFlags: NIIF_USER | NIIF_LARGE_ICON,
            hBalloonIcon: load_icon(SM_CXICON).unwrap_or_default(),
            ..Default::default()
        };
        copy_text(&mut data.szTip, title);
        copy_text(&mut data.szInfoTitle, title);
        copy_text(&mut data.szInfo, text);

        let shown = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
        let notification = Self { data };
        shown.then_some(notification)
    }
}

impl Drop for Notification {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
            let _ = DestroyIcon(self.data.hIcon);
            if !self.data.hBalloonIcon.is_invalid() {
                let _ = DestroyIcon(self.data.hBalloonIcon);
            }
        }
    }
}

/// Иконка программы из ресурсов exe нужного системного размера (`SM_CXSMICON` или `SM_CXICON`).
fn load_icon(size_metric: windows::Win32::UI::WindowsAndMessaging::SYSTEM_METRICS_INDEX) -> Option<HICON> {
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let size = GetSystemMetrics(size_metric);
        let name = PCWSTR(usize::from(ICON_RESOURCE_ID) as *const u16);
        let icon = LoadImageW(Some(module.into()), name, IMAGE_ICON, size, size, LR_DEFAULTCOLOR).ok()?;
        Some(HICON(icon.0))
    }
}

/// Копирует строку в буфер UTF-16 фиксированной длины, обрезая и завершая нулём.
fn copy_text(buffer: &mut [u16], text: &str) {
    let wide: Vec<u16> = text.encode_utf16().take(buffer.len() - 1).collect();
    buffer[..wide.len()].copy_from_slice(&wide);
    buffer[wide.len()] = 0;
}
