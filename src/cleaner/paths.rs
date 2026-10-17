//! Раскрытие шаблонов путей из каталога и защита от удаления важных папок.

use std::env;
use std::path::{Component, Path, PathBuf};

use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

/// Раскрывает шаблон вида `%LOCALAPPDATA%\Google\Chrome\User Data\*\Cache`
/// и возвращает все существующие пути, которые под него подходят.
///
/// Если какая-то переменная не задана (например, Steam не установлен),
/// возвращает пустой список — такой шаблон просто пропускается.
pub fn expand(pattern: &str) -> Vec<PathBuf> {
    let Some(expanded) = expand_variables(pattern) else {
        return Vec::new();
    };

    // Файловая система Windows не различает регистр — маски тоже не должны.
    let options = glob::MatchOptions { case_sensitive: false, ..Default::default() };

    match glob::glob_with(&expanded, options) {
        Ok(paths) => paths.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    }
}

/// Подставляет значения переменных `%NAME%`.
/// Значения экранируются, чтобы символы `[`, `]`, `*`, `?` в именах папок
/// не воспринимались как маска.
fn expand_variables(pattern: &str) -> Option<String> {
    let mut result = String::new();
    let mut rest = pattern;

    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let end = after_start.find('%')?;
        let value = variable_value(&after_start[..end])?;
        result.push_str(&glob::Pattern::escape(&value));
        rest = &after_start[end + 1..];
    }

    result.push_str(rest);
    Some(result)
}

/// Значение переменной окружения или одной из специальных переменных.
fn variable_value(name: &str) -> Option<String> {
    let value = match name {
        "STEAM" => steam_directory()?,
        _ => env::var(name).ok()?,
    };
    let value = value.trim().trim_end_matches(['\\', '/']).to_owned();
    (!value.is_empty()).then_some(value)
}

/// Папка установки Steam из реестра.
fn steam_directory() -> Option<String> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam").ok()?;
    let path: String = key.get_value("SteamPath").ok()?;
    Some(path.replace('/', "\\"))
}

/// Проверяет, что папку можно очищать.
///
/// Это последняя линия обороны на случай ошибки в каталоге или странных
/// переменных окружения (например, `TEMP=C:\Users\Имя`). Запрещены:
/// * корни дисков и папки первого уровня вроде `C:\Users`;
/// * важные системные и пользовательские папки и все их родители.
pub fn is_protected(path: &Path) -> bool {
    let depth = path.components().filter(|c| matches!(c, Component::Normal(_))).count();
    if depth < 1 {
        return true;
    }

    let path = normalize(path);
    protected_directories()
        .iter()
        .any(|protected| *protected == path || protected.starts_with(&format!("{path}\\")))
}

fn protected_directories() -> Vec<String> {
    let mut dirs: Vec<String> = [
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMDATA",
        "WINDIR",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "ONEDRIVE",
        "PUBLIC",
    ]
    .iter()
    .filter_map(|name| env::var(name).ok())
    .map(|dir| normalize(Path::new(&dir)))
    .collect();

    if let Ok(profile) = env::var("USERPROFILE") {
        for folder in ["Desktop", "Documents", "Downloads", "Pictures", "Music", "Videos", "Saved Games"] {
            dirs.push(normalize(&Path::new(&profile).join(folder)));
        }
    }
    if let Ok(windir) = env::var("WINDIR") {
        dirs.push(normalize(&Path::new(&windir).join("System32")));
    }
    dirs
}

/// Приводит путь к виду для сравнения: нижний регистр, без завершающего `\`.
fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_environment_variables() {
        let windir = env::var("WINDIR").unwrap();
        assert_eq!(expand_variables(r"%WINDIR%\Temp").unwrap(), format!(r"{windir}\Temp"));
        assert!(expand_variables(r"%HEXCLEANER_UNKNOWN_VARIABLE%\x").is_none());
    }

    #[test]
    fn expands_masks() {
        let windir = env::var("WINDIR").unwrap();
        assert_eq!(expand(r"%WINDIR%\System32"), vec![PathBuf::from(format!(r"{windir}\System32"))]);
        // Маски не зависят от регистра, как и файловая система Windows.
        assert!(expand(r"%WINDIR%\EXPLORER.*").iter().any(|path| path.ends_with("explorer.exe")));
        assert!(expand(r"%WINDIR%\no-such-folder-*").is_empty());
    }

    #[test]
    fn protects_important_directories() {
        let profile = env::var("USERPROFILE").unwrap();
        assert!(is_protected(Path::new(r"C:\")));
        assert!(is_protected(Path::new(&profile)));
        assert!(is_protected(&Path::new(&profile).join("Documents")));
        // Родитель профиля (C:\Users) тоже под защитой.
        assert!(is_protected(Path::new(&profile).parent().unwrap()));
        assert!(!is_protected(&Path::new(&profile).join(r"AppData\Local\Temp")));
    }
}
