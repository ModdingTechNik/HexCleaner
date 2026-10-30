//! Свои пункты очистки — JSON-файлы в `%APPDATA%\HexCleaner\tasks`, по одному набору на файл:
//!
//! ```json
//! {
//!   "tasks": [
//!     {
//!       "id": "gradle_caches",
//!       "title": { "ru": "Кэш Gradle", "en": "Gradle cache" },
//!       "description": "Скачанные зависимости и кэш сборки. Gradle скачает нужное заново.",
//!       "safety": "moderate",
//!       "actions": [
//!         { "clear": "%USERPROFILE%\\.gradle\\caches" },
//!         { "delete_files": "%USERPROFILE%\\.gradle\\daemon\\*\\*.log" }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! * `id` — латиница, цифры, `_` и `-`; по нему сохраняются галочки и его указывают
//!   в `--tasks`. Не должен совпадать со встроенным или чужим пунктом.
//! * `title`, `description` — строкой или по кодам языков, как названия тем.
//! * `safety` — `safe`, `moderate` (по умолчанию) или `caution`; `admin` — нужны права администратора.
//! * Действия — те же, что у встроенных пунктов, только файловые: `clear` (очистить папку,
//!   можно с `older_than_hours`), `delete_folder` (удалить папку целиком), `delete_files`.
//!   Пути — шаблоны с `%ПЕРЕМЕННЫМИ%` и `*`, как в каталоге.
//!
//! Файл — чужой код, поэтому пути проверяются строже, чем во встроенном каталоге: путь
//! должен быть полным, а его неизменная часть (до первой маски) — не защищённой папкой
//! (не корень диска, не профиль, не `C:\Windows` …). Пункт с ошибкой пропускается целиком,
//! а ошибка показывается в настройках окна и в `HexCleanerCli list`.
//! Образцы создаются только по просьбе пользователя (кнопка «Создать примеры») в подпапке
//! `examples`: её файлы не загружаются, пока их не скопируют в `tasks`. В отличие от языков
//! и тем, программе они не нужны, поэтому сами не появляются и после удаления не возвращаются.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::catalog::{Action, CleanupTask, CustomText, Safety};
use crate::cleaner::{is_protected, variable_value};
use crate::i18n::{self, LocalizedText};
use crate::t;

/// Образцы: имя файла и содержимое.
const EXAMPLES: [(&str, &str); 2] = [
    ("gradle.json", include_str!("../tasks/examples/gradle.json")),
    ("claude.json", include_str!("../tasks/examples/claude.json")),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskFile {
    tasks: Vec<TaskDefinition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskDefinition {
    id: String,
    title: LocalizedText,
    #[serde(default)]
    description: Option<LocalizedText>,
    #[serde(default)]
    safety: SafetyName,
    #[serde(default)]
    admin: bool,
    actions: Vec<ActionDefinition>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SafetyName {
    Safe,
    #[default]
    Moderate,
    Caution,
}

/// Действие: ровно одно из `clear`, `delete_folder`, `delete_files`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionDefinition {
    #[serde(default)]
    clear: Option<String>,
    #[serde(default)]
    delete_folder: Option<String>,
    #[serde(default)]
    delete_files: Option<String>,
    /// Только для `clear`: не трогать файлы и папки моложе стольких часов.
    #[serde(default)]
    older_than_hours: Option<f64>,
}

/// Папка своих пунктов.
pub fn tasks_directory() -> PathBuf {
    let app_data = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(app_data).join("HexCleaner").join("tasks")
}

/// Записывает образцы в `tasks\examples` и возвращает эту папку. Существующие файлы
/// не перезаписываются: пользователь мог их отредактировать.
pub fn create_examples() -> std::io::Result<PathBuf> {
    let dir = tasks_directory().join("examples");
    fs::create_dir_all(&dir)?;
    for (name, json) in EXAMPLES {
        let path = dir.join(name);
        if !path.exists() {
            fs::write(path, json)?;
        }
    }
    Ok(dir)
}

/// Свои пункты из папки своих пунктов (в порядке имён файлов) и ошибки в файлах.
/// `taken_ids` — идентификаторы встроенных пунктов: свои не должны с ними совпадать.
pub fn load(taken_ids: &[&str]) -> (Vec<CleanupTask>, Vec<String>) {
    load_from(&tasks_directory(), taken_ids)
}

fn load_from(dir: &Path, taken_ids: &[&str]) -> (Vec<CleanupTask>, Vec<String>) {
    let mut files = i18n::json_files(dir);
    files.sort();
    let mut taken: BTreeSet<String> = taken_ids.iter().map(|id| id.to_string()).collect();
    let (mut tasks, mut errors) = (Vec::new(), Vec::new());

    for path in files {
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let definitions = match read_file(&path) {
            Ok(definitions) => definitions,
            Err(error) => {
                errors.push(t!("custom_task.error.file", file = file, error = error));
                continue;
            }
        };
        for definition in definitions {
            let id = definition.id.clone();
            match build(definition, &file, &taken) {
                Ok(task) => {
                    taken.insert(task.id.to_owned());
                    tasks.push(task);
                }
                Err(error) => errors.push(t!("custom_task.error.task", file = file, id = id, error = error)),
            }
        }
    }
    (tasks, errors)
}

fn read_file(path: &Path) -> Result<Vec<TaskDefinition>, String> {
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    // Блокнот Windows может сохранить файл с BOM — он не мешает.
    let file: TaskFile = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|error| error.to_string())?;
    Ok(file.tasks)
}

/// Пункт каталога из описания в файле `file`. `taken` — уже занятые идентификаторы.
fn build(definition: TaskDefinition, file: &str, taken: &BTreeSet<String>) -> Result<CleanupTask, String> {
    let id = definition.id.trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err(t!("custom_task.error.id"));
    }
    if taken.contains(id) {
        return Err(t!("custom_task.error.duplicate_id"));
    }
    if definition.actions.is_empty() {
        return Err(t!("custom_task.error.no_actions"));
    }
    let actions = definition.actions.into_iter().map(build_action).collect::<Result<Vec<_>, _>>()?;

    Ok(CleanupTask {
        id: intern(id),
        safety: match definition.safety {
            SafetyName::Safe => Safety::Safe,
            SafetyName::Moderate => Safety::Moderate,
            SafetyName::Caution => Safety::Caution,
        },
        programs_to_close: &[],
        requires_admin: definition.admin,
        services_to_stop: &[],
        custom: Some(CustomText { title: definition.title, description: definition.description, file: file.to_owned() }),
        actions,
    })
}

fn build_action(definition: ActionDefinition) -> Result<Action, String> {
    let action = match (definition.clear, definition.delete_folder, definition.delete_files) {
        (Some(pattern), None, None) => {
            let min_age = match definition.older_than_hours {
                Some(hours) if hours.is_finite() && hours >= 0.0 => Some(Duration::from_secs_f64(hours * 3600.0)),
                Some(_) => return Err(t!("custom_task.error.older_than")),
                None => None,
            };
            Action::ClearDirectory { pattern, min_age }
        }
        (None, Some(pattern), None) => Action::DeleteDirectory { pattern },
        (None, None, Some(pattern)) => Action::DeleteFiles { pattern },
        _ => return Err(t!("custom_task.error.action")),
    };
    if definition.older_than_hours.is_some() && !matches!(action, Action::ClearDirectory { .. }) {
        return Err(t!("custom_task.error.older_than"));
    }
    let (pattern, is_file) = match &action {
        Action::ClearDirectory { pattern, .. } | Action::DeleteDirectory { pattern } => (pattern, false),
        Action::DeleteFiles { pattern } => (pattern, true),
        // Своим пунктам доступны только файловые действия — других здесь не бывает.
        _ => unreachable!(),
    };
    check_pattern(pattern, is_file)?;
    Ok(action)
}

/// Путь полный, а его неизменная часть — до первой маски, у файла без маски — его папка —
/// не защищённая папка: иначе, например, `D:\*` или `%USERPROFILE%\*` очистили бы слишком много.
fn check_pattern(pattern: &str, is_file: bool) -> Result<(), String> {
    // Переменная не задана (например, программа не установлена) — шаблон просто ни к чему не подойдёт.
    let Some(expanded) = expand_variables(pattern) else { return Ok(()) };
    let path = Path::new(&expanded);
    if !path.is_absolute() {
        return Err(t!("custom_task.error.relative", pattern = pattern));
    }
    let mut fixed = PathBuf::new();
    let mut has_mask = false;
    for component in path.components() {
        if let Component::Normal(part) = component
            && part.to_string_lossy().contains(['*', '?', '['])
        {
            has_mask = true;
            break;
        }
        fixed.push(component);
    }
    if is_file && !has_mask {
        fixed.pop();
    }
    if fixed.components().any(|component| matches!(component, Component::ParentDir)) || is_protected(&fixed) {
        return Err(t!("custom_task.error.too_broad", pattern = pattern));
    }
    Ok(())
}

/// `%NAME%` → значение переменной, без экранирования масок; `None`, если переменная не задана.
fn expand_variables(pattern: &str) -> Option<String> {
    let mut result = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let end = after_start.find('%')?;
        result.push_str(&variable_value(&after_start[..end])?);
        rest = &after_start[end + 1..];
    }
    result.push_str(rest);
    Some(result)
}

/// Идентификатор с временем жизни программы. Каталог хранит `&'static str`, а свои пункты
/// читаются из файлов заново при каждом обращении — каждое имя запоминается один раз.
fn intern(id: &str) -> &'static str {
    static IDS: LazyLock<Mutex<BTreeSet<&'static str>>> = LazyLock::new(Default::default);
    let mut ids = IDS.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(&known) = ids.get(id) {
        return known;
    }
    let leaked: &'static str = Box::leak(id.to_owned().into_boxed_str());
    ids.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hexcleaner_custom_tasks_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn examples_are_valid() {
        let dir = temp_dir("examples");
        for (name, json) in EXAMPLES {
            fs::write(dir.join(name), json).unwrap();
        }
        let (tasks, errors) = load_from(&dir, &[]);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(tasks.len() >= EXAMPLES.len());
        assert!(tasks.iter().all(|task| task.deletes_files() && task.custom.is_some()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_tasks_and_reports_errors() {
        let dir = temp_dir("load");
        fs::write(
            dir.join("a.json"),
            r#"{"tasks": [
                {"id": "my_cache", "title": {"ru": "Мой кэш", "en": "My cache"}, "safety": "safe",
                 "actions": [{"clear": "%TEMP%\\my-app", "older_than_hours": 2}, {"delete_files": "%TEMP%\\my-app-*.log"}]},
                {"id": "user_temp", "title": "Повтор встроенного", "actions": [{"clear": "%TEMP%\\x"}]},
                {"id": "Bad Id", "title": "Плохой id", "actions": [{"clear": "%TEMP%\\x"}]},
                {"id": "everything", "title": "Весь профиль", "actions": [{"clear": "%USERPROFILE%\\*"}]},
                {"id": "drive", "title": "Диск", "actions": [{"delete_folder": "C:\\*"}]},
                {"id": "relative", "title": "Без диска", "actions": [{"clear": "cache"}]},
                {"id": "two", "title": "Два действия", "actions": [{"clear": "%TEMP%\\x", "delete_files": "%TEMP%\\y"}]},
                {"id": "age", "title": "Возраст", "actions": [{"delete_files": "%TEMP%\\*.log", "older_than_hours": 1}]},
                {"id": "empty", "title": "Пусто", "actions": []}
            ]}"#,
        )
        .unwrap();
        // Тот же id в другом файле — второй пропускается.
        fs::write(dir.join("b.json"), r#"{"tasks": [{"id": "my_cache", "title": "Ещё раз", "actions": [{"clear": "%TEMP%\\z"}]}]}"#).unwrap();
        fs::write(dir.join("c.json"), r#"{"tasks": [{"id": "x", "title": "x", "actions": [], "colour": "red"}]}"#).unwrap();

        let (tasks, errors) = load_from(&dir, &["user_temp"]);
        assert_eq!(tasks.len(), 1, "{errors:?}");
        let task = &tasks[0];
        assert_eq!((task.id, task.safety, task.requires_admin), ("my_cache", Safety::Safe, false));
        assert!(matches!(task.actions[0], Action::ClearDirectory { min_age: Some(age), .. } if age == Duration::from_secs(7200)));
        assert_eq!(task.custom.as_ref().unwrap().file, "a.json");
        assert!(task.description().contains("a.json"));
        // Восемь плохих пунктов в a.json, повтор в b.json и лишнее поле в c.json.
        assert_eq!(errors.len(), 10, "{errors:#?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn checks_patterns() {
        let profile = std::env::var("USERPROFILE").unwrap();
        assert!(check_pattern(r"%USERPROFILE%\.gradle\caches", false).is_ok());
        assert!(check_pattern(r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Cache", false).is_ok());
        assert!(check_pattern(r"%LOCALAPPDATA%\Temp\*.log", true).is_ok());
        assert!(check_pattern(&format!(r"{profile}\.gradle\caches"), false).is_ok());
        // Переменной нет — шаблон ни к чему не подойдёт, ошибки нет.
        assert!(check_pattern(r"%HEXCLEANER_NO_SUCH_VARIABLE%\x", false).is_ok());

        for (pattern, is_file) in [
            (r"%USERPROFILE%", false),
            (r"%USERPROFILE%\*", false),
            (r"%USERPROFILE%\Documents", false),
            (r"C:\*", false),
            (r"%WINDIR%\*", false),
            (r"%USERPROFILE%\notes.txt", true),
            (r"%USERPROFILE%\.gradle\..\..", false),
            (r"cache\*", false),
        ] {
            assert!(check_pattern(pattern, is_file).is_err(), "{pattern} принят");
        }
    }

    #[test]
    fn interned_ids_are_shared() {
        assert!(std::ptr::eq(intern("same_id"), intern("same_id")));
    }
}
