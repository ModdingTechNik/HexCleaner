//! Обёртки над Windows API. Весь `unsafe` код программы собран здесь.

pub mod admin;
pub mod console;
pub mod delete;
pub mod event_log;
pub mod locale;
pub mod memory;
pub mod recycle_bin;
pub mod restart_manager;
pub mod services;

use windows::Win32::Foundation::{CloseHandle, HANDLE};

/// Дескриптор Windows, который автоматически закрывается при выходе из области видимости.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
