//! Корзина Windows текущего пользователя (на всех дисках).

use std::mem::size_of;

use windows::Win32::UI::Shell::{
    SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW, SHQUERYRBINFO, SHQueryRecycleBinW,
};
use windows::core::{PCWSTR, Result};

/// Сколько лежит в корзине.
pub struct RecycleBinInfo {
    pub items: u64,
    pub bytes: u64,
}

/// Число объектов в корзине и их общий размер.
pub fn query() -> Result<RecycleBinInfo> {
    let mut info = SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // Пустой путь — корзины всех дисков сразу.
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info)? };
    Ok(RecycleBinInfo { items: info.i64NumItems.max(0) as u64, bytes: info.i64Size.max(0) as u64 })
}

/// Безвозвратно очищает корзину на всех дисках — без вопросов, окна прогресса и звука.
pub fn empty() -> Result<()> {
    unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Только чтение: корзину пользователя тест не очищает.
    #[test]
    fn query_reports_recycle_bin_contents() {
        let info = query().expect("SHQueryRecycleBinW должен работать");
        assert!(info.items > 0 || info.bytes == 0);
    }
}
