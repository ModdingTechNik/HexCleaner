//! Сжатие NTFS: файлы сжимает файловая система, как галочка «Сжимать содержимое для экономии
//! места» в свойствах папки. Пути и имена не меняются, читать файлы можно как обычно.
//!
//! Перенос файла внутри диска его не сжимает, поэтому папку целиком сжимает стандартная
//! программа Windows `compact.exe`. Она же помечает папку, чтобы новые файлы в ней сжимались сами.

use std::io;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// Запустить программу без окна консоли.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Сжимает папку `dir` со всем содержимым в фоне: не дожидается окончания.
/// На дисках без NTFS (FAT32, exFAT) `compact` просто ничего не сделает.
pub fn compress_in_background(dir: &Path) -> io::Result<()> {
    Command::new("compact")
        .args(["/c", "/s", "/i", "/q"])
        .current_dir(dir)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}
