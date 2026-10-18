//! Язык интерфейса Windows.

use windows::Win32::Globalization::GetUserDefaultUILanguage;

/// Основной код языка интерфейса Windows (младшие 10 бит LANGID), например `0x19` — русский.
pub fn ui_language_primary_id() -> u16 {
    unsafe { GetUserDefaultUILanguage() & 0x3FF }
}
