//! Локализация: все тексты программы берутся отсюда по ключу.
//!
//! В коде тексты не пишутся — вместо них ключи: `t!("footer.analyze")` или с подстановками
//! `t!("status.clean_done", files = 10, size = "1.5 МБ")`. Сами тексты лежат в JSON-файлах:
//!
//! ```json
//! { "name": "Русский", "strings": { "footer.analyze": "Анализ", ... } }
//! ```
//!
//! * Встроенные языки (`languages/*.json` в исходниках) вшиты в программу. При запуске они
//!   записываются в папку языков `%APPDATA%\HexCleaner\languages`, если их там ещё нет, —
//!   как образец для своих переводов.
//! * Любой другой `*.json` из этой папки тоже появляется в списке языков; код языка — имя файла.
//! * Файл с именем встроенного языка (например, `ru.json`) дополняет и переопределяет его тексты.
//! * Если в переводе нет какого-то ключа, берётся английский текст.

use std::collections::HashMap;
use std::fmt::{self, Display};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use serde::Deserialize;

/// Текст по ключу из текущего языка: `t!("footer.analyze")`,
/// или с подстановками `{имя}`: `t!("status.clean_done", files = 10, size = "1.5 МБ")`.
#[macro_export]
macro_rules! t {
    ($key:expr) => {
        $crate::i18n::text($key)
    };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::format($key, &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+])
    };
}

/// Встроенные языки: код (он же имя файла) и содержимое JSON.
const BUILT_IN: [(&str, &str); 3] = [
    ("ru", include_str!("../languages/ru.json")),
    ("uk", include_str!("../languages/uk.json")),
    ("en", include_str!("../languages/en.json")),
];

/// Язык, из которого берутся тексты, которых нет в выбранном переводе.
const FALLBACK_LANGUAGE: &str = "en";

/// Язык по умолчанию, пока не выбран другой.
const DEFAULT_LANGUAGE: &str = "ru";

/// Содержимое файла перевода.
#[derive(Deserialize)]
struct LanguageFile {
    /// Название языка в списке выбора, например «Deutsch».
    name: String,
    /// Тексты по ключам.
    strings: HashMap<String, String>,
}

/// Язык в списке выбора.
#[derive(Clone, Debug)]
pub struct LanguageInfo {
    /// Код языка — имя файла без `.json`, например `de`.
    pub code: String,
    pub name: String,
}

/// Почему не удалось загрузить язык.
#[derive(Debug)]
pub enum LanguageError {
    NotFound,
    Invalid { file: String, error: String },
}

impl Display for LanguageError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LanguageError::NotFound => f.write_str(&t!("language.not_found")),
            LanguageError::Invalid { file, error } => f.write_str(&t!("language.invalid", file = file, error = error)),
        }
    }
}

/// Загруженный перевод вместе с запасным языком.
struct Translation {
    strings: HashMap<String, String>,
    fallback: HashMap<String, String>,
}

static CURRENT: LazyLock<RwLock<Arc<Translation>>> = LazyLock::new(|| {
    let translation = Translation {
        strings: built_in(DEFAULT_LANGUAGE).map(|file| file.strings).unwrap_or_default(),
        fallback: built_in(FALLBACK_LANGUAGE).map(|file| file.strings).unwrap_or_default(),
    };
    RwLock::new(Arc::new(translation))
});

/// Текст по ключу. Если ключа нет ни в текущем, ни в запасном языке — возвращается сам ключ,
/// чтобы пропуск был сразу виден в интерфейсе.
pub fn text(key: &str) -> String {
    let translation = Arc::clone(&CURRENT.read().unwrap_or_else(|error| error.into_inner()));
    translation
        .strings
        .get(key)
        .or_else(|| translation.fallback.get(key))
        .cloned()
        .unwrap_or_else(|| key.to_owned())
}

/// Текст по ключу с подстановкой значений вместо `{имя}`.
pub fn format(key: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut result = text(key);
    for (name, value) in args {
        result = result.replace(&format!("{{{name}}}"), &value.to_string());
    }
    result
}

/// Переключает язык всей программы.
pub fn set_language(code: &str) -> Result<(), LanguageError> {
    let translation = load(code, &languages_directory())?;
    *CURRENT.write().unwrap_or_else(|error| error.into_inner()) = Arc::new(translation);
    Ok(())
}

/// Включает язык `code` или, если он не задан, язык Windows.
/// Если файла языка нет или он испорчен, включает язык Windows и возвращает текст ошибки.
pub fn select_language(code: Option<&str>) -> Option<String> {
    let code = match code {
        Some(code) => code,
        None => system_language(),
    };
    match set_language(code) {
        Ok(()) => None,
        Err(error) => {
            let _ = set_language(system_language());
            Some(t!("status.language_failed", code = code, error = error))
        }
    }
}

/// Язык интерфейса Windows, если для него есть встроенный перевод, иначе английский.
pub fn system_language() -> &'static str {
    const LANG_RUSSIAN: u16 = 0x19;
    const LANG_UKRAINIAN: u16 = 0x22;
    match crate::system::locale::ui_language_primary_id() {
        LANG_RUSSIAN => "ru",
        LANG_UKRAINIAN => "uk",
        _ => "en",
    }
}

/// Папка с файлами переводов.
pub fn languages_directory() -> PathBuf {
    let app_data = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(app_data).join("HexCleaner").join("languages")
}

/// Записывает встроенные языки в папку языков как образцы. Существующие файлы
/// не перезаписываются: пользователь мог их отредактировать.
pub fn export_built_in_languages() {
    let dir = languages_directory();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (code, json) in BUILT_IN {
        let path = dir.join(format!("{code}.json"));
        if !path.exists() {
            let _ = fs::write(path, json);
        }
    }
}

/// Все доступные языки (встроенные и из папки языков), отсортированные по названию,
/// и список файлов, которые не удалось прочитать.
pub fn available_languages() -> (Vec<LanguageInfo>, Vec<String>) {
    let mut languages: HashMap<String, String> = BUILT_IN
        .iter()
        .filter_map(|(code, _)| Some((code.to_string(), built_in(code)?.name)))
        .collect();
    let mut broken = Vec::new();

    for path in language_files(&languages_directory()) {
        let Some(code) = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()) else { continue };
        match read_language_file(&path) {
            Ok(file) => {
                languages.insert(code, file.name);
            }
            Err(_) => broken.push(path.file_name().unwrap_or_default().to_string_lossy().into_owned()),
        }
    }

    let mut list: Vec<LanguageInfo> = languages.into_iter().map(|(code, name)| LanguageInfo { code, name }).collect();
    list.sort_by_key(|language| language.name.to_lowercase());
    (list, broken)
}

/// Перевод для языка `code`: встроенные тексты, поверх них — тексты из файла в `dir`.
fn load(code: &str, dir: &Path) -> Result<Translation, LanguageError> {
    let mut strings = HashMap::new();
    let mut found = false;

    if let Some(file) = built_in(code) {
        strings.extend(file.strings);
        found = true;
    }
    let path = dir.join(format!("{code}.json"));
    if path.is_file() {
        let file = read_language_file(&path)?;
        strings.extend(file.strings);
        found = true;
    }

    if !found {
        return Err(LanguageError::NotFound);
    }
    let fallback = built_in(FALLBACK_LANGUAGE).map(|file| file.strings).unwrap_or_default();
    Ok(Translation { strings, fallback })
}

fn built_in(code: &str) -> Option<LanguageFile> {
    let (_, json) = BUILT_IN.iter().find(|(built_in_code, _)| *built_in_code == code)?;
    serde_json::from_str(json).ok()
}

fn read_language_file(path: &Path) -> Result<LanguageFile, LanguageError> {
    let invalid = |error: String| LanguageError::Invalid {
        file: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        error,
    };
    let text = fs::read_to_string(path).map_err(|error| invalid(error.to_string()))?;
    // Блокнот Windows может сохранить файл с BOM — он не мешает.
    serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|error| invalid(error.to_string()))
}

fn language_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("json")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys(code: &str) -> BTreeSet<String> {
        built_in(code).expect("встроенный язык должен читаться").strings.into_keys().collect()
    }

    /// Исходники программы одной строкой.
    fn source_code() -> String {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        rust_files(&src).iter().map(|file| fs::read_to_string(file).unwrap()).collect()
    }

    /// Все ключи из вызовов макроса `t!` (но не `format!`) и ключи пунктов каталога.
    fn keys_used_in_code(code: &str) -> BTreeSet<String> {
        let mut used = BTreeSet::new();
        for (start, _) in code.match_indices("t!(\"") {
            let before = code[..start].chars().next_back().unwrap_or(' ');
            if before.is_alphanumeric() || before == '_' {
                continue;
            }
            let key = code[start + 4..].split('"').next().unwrap();
            used.insert(key.to_owned());
        }
        for task in crate::catalog::all_tasks() {
            used.insert(format!("task.{}.title", task.id));
            used.insert(format!("task.{}.description", task.id));
        }
        used
    }

    fn rust_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for entry in fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                files.extend(rust_files(&path));
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
        files
    }

    #[test]
    fn built_in_languages_have_the_same_keys() {
        for (code, _) in BUILT_IN {
            assert_eq!(keys(code), keys("ru"), "в языке {code} другой набор ключей");
        }
    }

    #[test]
    fn every_used_key_is_translated_and_no_key_is_unused() {
        let code = source_code();
        let used = keys_used_in_code(&code);
        let translated = keys("ru");
        let missing: Vec<_> = used.difference(&translated).collect();
        // Ключ может передаваться и через переменную (как единицы измерения в `format_size`),
        // поэтому неиспользуемым считаем ключ, который не встречается в коде вообще.
        let unused: Vec<_> = translated
            .difference(&used)
            .filter(|key| !code.contains(&format!("\"{key}\"")))
            .collect();
        assert!(missing.is_empty(), "нет перевода для ключей: {missing:?}");
        assert!(unused.is_empty(), "ключи не используются в коде: {unused:?}");
    }

    /// Все тексты интерфейса, журнала и ошибок должны идти через переводы:
    /// русских строк в коде быть не должно (комментарии и тесты не в счёт).
    #[test]
    fn no_hardcoded_russian_text_in_code() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for file in rust_files(&src) {
            let code = fs::read_to_string(&file).unwrap();
            let without_tests = code.split("#[cfg(test)]").next().unwrap();
            for (number, line) in without_tests.lines().enumerate() {
                let code_part = line.split("//").next().unwrap();
                if code_part.chars().any(|c| ('а'..='я').contains(&c.to_lowercase().next().unwrap()) || c == 'ё') {
                    offenders.push(format!("{}:{}: {}", file.display(), number + 1, line.trim()));
                }
            }
        }
        assert!(offenders.is_empty(), "текст без перевода:\n{}", offenders.join("\n"));
    }

    #[test]
    fn user_file_overrides_built_in_and_falls_back_to_english() {
        let dir = std::env::temp_dir().join(format!("hexcleaner_languages_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("de.json"),
            "\u{feff}{ \"name\": \"Deutsch\", \"strings\": { \"footer.analyze\": \"Analyse {x}\" } }",
        )
        .unwrap();
        fs::write(dir.join("ru.json"), r#"{ "name": "Русский", "strings": { "footer.stop": "Стоп" } }"#).unwrap();

        let german = load("de", &dir).unwrap();
        assert_eq!(german.strings["footer.analyze"], "Analyse {x}");
        assert!(!german.strings.contains_key("footer.stop"));
        assert_eq!(german.fallback["footer.stop"], "Stop");

        // Свой ru.json дополняет встроенный русский, а не заменяет его целиком.
        let russian = load("ru", &dir).unwrap();
        assert_eq!(russian.strings["footer.stop"], "Стоп");
        assert_eq!(russian.strings["footer.analyze"], "Анализ");

        assert!(matches!(load("xx", &dir), Err(LanguageError::NotFound)));
        fs::write(dir.join("broken.json"), "{ not json").unwrap();
        assert!(matches!(load("broken", &dir), Err(LanguageError::Invalid { .. })));

        let _ = fs::remove_dir_all(&dir);
    }
}
