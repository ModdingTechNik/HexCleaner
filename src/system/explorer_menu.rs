//! Пункт «Анализ папки в HexCleaner» в контекстном меню проводника: для папок, фона
//! открытой папки и дисков. Пишется в реестр текущего пользователя (`HKCU\Software\Classes`),
//! поэтому права администратора не нужны.
//!
//! Пункт запускает `HexCleaner.exe --disk-space "<папка>"`. С флагом «только с Shift»
//! (значение `Extended`) он виден, только если щёлкнуть правой кнопкой с нажатым Shift.
//! В Windows 11 такие пункты показываются в «Показать дополнительные параметры».

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

/// Параметр `HexCleaner.exe`, с которым он открывает папку во вкладке «Место на диске».
pub const OPEN_FLAG: &str = "--disk-space";

/// Где регистрируется пункт и как проводник подставляет путь:
/// `%1` — папка или диск, на котором щёлкнули, `%V` — открытая папка (щелчок по фону).
const KEYS: [(&str, &str); 3] = [
    (r"Software\Classes\Directory\shell\HexCleaner", "%1"),
    (r"Software\Classes\Directory\Background\shell\HexCleaner", "%V"),
    (r"Software\Classes\Drive\shell\HexCleaner", "%1"),
];

/// Пункт меню сейчас.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuState {
    pub installed: bool,
    /// Виден только при щелчке с Shift.
    pub shift_only: bool,
    /// Программа, которую запускает пункт.
    pub exe: Option<PathBuf>,
}

/// Читает пункт из реестра (по ключу для папок — остальные пишутся вместе с ним).
pub fn state() -> MenuState {
    let user = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(key) = user.open_subkey(KEYS[0].0) else { return MenuState::default() };
    let command: String = key.open_subkey("command").and_then(|command| command.get_value("")).unwrap_or_default();
    MenuState {
        installed: true,
        shift_only: key.get_raw_value("Extended").is_ok(),
        exe: exe_from_command(&command),
    }
}

/// Добавляет или обновляет пункт: `label` — его текст, `exe` — программа, которую он запускает.
pub fn install(exe: &Path, label: &str, shift_only: bool) -> io::Result<()> {
    let user = RegKey::predef(HKEY_CURRENT_USER);
    let icon = format!("\"{}\",0", exe.display());
    for (path, placeholder) in KEYS {
        let (key, _) = user.create_subkey(path)?;
        key.set_value("", &label)?;
        key.set_value("Icon", &icon)?;
        if shift_only {
            key.set_value("Extended", &"")?;
        } else if key.get_raw_value("Extended").is_ok() {
            key.delete_value("Extended")?;
        }
        let (command, _) = key.create_subkey("command")?;
        command.set_value("", &command_line(exe, placeholder))?;
    }
    notify_explorer();
    Ok(())
}

/// Убирает пункт из меню.
pub fn uninstall() -> io::Result<()> {
    let user = RegKey::predef(HKEY_CURRENT_USER);
    for (path, _) in KEYS {
        match user.delete_subkey_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    notify_explorer();
    Ok(())
}

/// Путь из командной строки проводника. Проводник подставляет корень диска как `"C:\"`,
/// а по правилам командной строки Windows `\"` — это кавычка, поэтому приходит `C:"`.
pub fn path_from_argument(argument: &str) -> PathBuf {
    let path = argument.trim().trim_end_matches('"');
    if path.len() == 2 && path.ends_with(':') { PathBuf::from(format!("{path}\\")) } else { PathBuf::from(path) }
}

fn command_line(exe: &Path, placeholder: &str) -> String {
    format!("\"{}\" {OPEN_FLAG} \"{placeholder}\"", exe.display())
}

/// `"C:\…\HexCleaner.exe" --disk-space "%1"` → `C:\…\HexCleaner.exe`.
fn exe_from_command(command: &str) -> Option<PathBuf> {
    let exe = command.strip_prefix('"')?.split('"').next()?;
    (!exe.is_empty()).then(|| PathBuf::from(exe))
}

/// Просит проводник перечитать меню.
fn notify_explorer() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_round_trip() {
        let exe = Path::new(r"C:\Program Files\HexCleaner\HexCleaner.exe");
        let command = command_line(exe, "%1");
        assert_eq!(command, r#""C:\Program Files\HexCleaner\HexCleaner.exe" --disk-space "%1""#);
        assert_eq!(exe_from_command(&command).as_deref(), Some(exe));
        assert_eq!(exe_from_command("garbage"), None);
    }

    #[test]
    fn drive_root_argument_is_repaired() {
        assert_eq!(path_from_argument("C:\""), PathBuf::from(r"C:\"));
        assert_eq!(path_from_argument(r"C:\"), PathBuf::from(r"C:\"));
        assert_eq!(path_from_argument("D:"), PathBuf::from(r"D:\"));
        assert_eq!(path_from_argument(r"C:\Users\Me"), PathBuf::from(r"C:\Users\Me"));
    }
}
