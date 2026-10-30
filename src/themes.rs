//! Темы оформления — JSON-файлы с цветами окна, устроенные как переводы (см. [`crate::i18n`]):
//!
//! ```json
//! {
//!   "name": { "ru": "Тёмная", "en": "Dark" },
//!   "base": "dark",
//!   "colors": { "background": "#1B1B1B", "accent": "#005C80", ... },
//!   "chart": ["#4E79A7", "#F28E2B", ...]
//! }
//! ```
//!
//! * `name` — название в списке тем: строкой или по кодам языков (берётся язык программы,
//!   затем английский). Без него — имя файла.
//! * `base` — `dark` или `light`: от какой темы egui взять цвета, которых нет в `colors`.
//! * `colors` — цвета `#RRGGBB` или `#RRGGBBAA`; какие бывают и где видны — в README
//!   и в `src/theme.rs` окна. `chart` — цвета секторов на вкладке «Место на диске».
//!
//! * Встроенные темы (`themes/*.json` в исходниках) вшиты в программу. При запуске окна они
//!   записываются в папку тем `%APPDATA%\HexCleaner\themes`, если их там ещё нет, — как образцы.
//! * Любой другой `*.json` из этой папки тоже появляется в списке тем; код темы — имя файла.
//! * Файл с именем встроенной темы (например, `dark.json`) дополняет и переопределяет её цвета.
//!
//! Здесь — только файлы и список тем, без egui: тему выбирает и командная строка.

use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::i18n::{self, LocalizedText};
use crate::t;

/// Тема «как в Windows»: тёмная или светлая встроенная, смотря по настройке системы.
pub const SYSTEM: &str = "system";

/// Тема по умолчанию.
pub const DEFAULT: &str = "dark";

/// Встроенные темы: код (он же имя файла) и содержимое JSON.
const BUILT_IN: [(&str, &str); 2] =
    [("dark", include_str!("../themes/dark.json")), ("light", include_str!("../themes/light.json"))];

/// Основа темы: тёмная или светлая.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Base {
    #[default]
    Dark,
    Light,
}

/// Содержимое файла темы. Все поля можно не указывать.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ThemeFile {
    pub name: Option<LocalizedText>,
    pub base: Option<Base>,
    /// Цвета по названиям: `"background": "#1B1B1B"`.
    pub colors: BTreeMap<String, String>,
    /// Цвета секторов на вкладке «Место на диске», по кругу.
    pub chart: Vec<String>,
}

impl ThemeFile {
    pub fn base(&self) -> Base {
        self.base.unwrap_or_default()
    }

    /// Файл `other` поверх этого: его цвета заменяют и дополняют здешние.
    fn extend(&mut self, other: ThemeFile) {
        if other.name.is_some() {
            self.name = other.name;
        }
        if other.base.is_some() {
            self.base = other.base;
        }
        self.colors.extend(other.colors);
        if !other.chart.is_empty() {
            self.chart = other.chart;
        }
    }
}

/// Тема в списке выбора.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeInfo {
    /// Код темы — имя файла без `.json`, например `ocean`.
    pub id: String,
    pub name: String,
}

/// Почему не удалось загрузить тему.
#[derive(Debug)]
pub enum ThemeError {
    NotFound,
    Invalid { file: String, error: String },
}

impl Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ThemeError::NotFound => f.write_str(&t!("theme.not_found")),
            ThemeError::Invalid { file, error } => f.write_str(&t!("theme.invalid", file = file, error = error)),
        }
    }
}

/// Папка с файлами тем.
pub fn themes_directory() -> PathBuf {
    let app_data = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(app_data).join("HexCleaner").join("themes")
}

/// Записывает встроенные темы в папку тем как образцы. Существующие файлы
/// не перезаписываются: пользователь мог их отредактировать.
pub fn export_built_in_themes() {
    let dir = themes_directory();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (id, json) in BUILT_IN {
        let path = dir.join(format!("{id}.json"));
        if !path.exists() {
            let _ = fs::write(path, json);
        }
    }
}

/// Все доступные темы (встроенные и из папки тем), отсортированные по названию,
/// и список файлов, которые не удалось прочитать.
pub fn available_themes() -> (Vec<ThemeInfo>, Vec<String>) {
    available_in(&themes_directory())
}

fn available_in(dir: &Path) -> (Vec<ThemeInfo>, Vec<String>) {
    let mut themes: BTreeMap<String, Option<LocalizedText>> =
        BUILT_IN.iter().map(|(id, _)| (id.to_string(), built_in(id).and_then(|file| file.name))).collect();
    let mut broken = Vec::new();

    for path in i18n::json_files(dir) {
        let Some(id) = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()) else { continue };
        match read_theme_file(&path) {
            Ok(file) => {
                let name = themes.entry(id).or_default();
                if file.name.is_some() {
                    *name = file.name;
                }
            }
            Err(_) => broken.push(path.file_name().unwrap_or_default().to_string_lossy().into_owned()),
        }
    }

    let mut list: Vec<ThemeInfo> = themes
        .into_iter()
        .map(|(id, name)| ThemeInfo { name: name.and_then(|name| name.resolve()).unwrap_or_else(|| id.clone()), id })
        .collect();
    list.sort_by_key(|theme| theme.name.to_lowercase());
    (list, broken)
}

/// Тема `id`: встроенные цвета, поверх них — цвета из файла в папке тем.
pub fn load(id: &str) -> Result<ThemeFile, ThemeError> {
    load_from(id, &themes_directory())
}

fn load_from(id: &str, dir: &Path) -> Result<ThemeFile, ThemeError> {
    let mut theme = built_in(id);
    let path = dir.join(format!("{id}.json"));
    if path.is_file() {
        let file = read_theme_file(&path)?;
        match &mut theme {
            Some(theme) => theme.extend(file),
            None => theme = Some(file),
        }
    }
    theme.ok_or(ThemeError::NotFound)
}

fn built_in(id: &str) -> Option<ThemeFile> {
    let (_, json) = BUILT_IN.iter().find(|(built_in_id, _)| *built_in_id == id)?;
    serde_json::from_str(json).ok()
}

fn read_theme_file(path: &Path) -> Result<ThemeFile, ThemeError> {
    let invalid = |error: String| ThemeError::Invalid {
        file: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        error,
    };
    let text = fs::read_to_string(path).map_err(|error| invalid(error.to_string()))?;
    // Блокнот Windows может сохранить файл с BOM — он не мешает.
    serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|error| invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_themes_parse() {
        for (id, _) in BUILT_IN {
            let theme = built_in(id).unwrap_or_else(|| panic!("встроенная тема {id} не читается"));
            assert!(theme.name.is_some(), "у темы {id} нет названия");
        }
        assert_eq!(built_in("dark").unwrap().base(), Base::Dark);
        assert_eq!(built_in("light").unwrap().base(), Base::Light);
    }

    #[test]
    fn user_files_add_themes_and_override_built_in_colors() {
        let dir = std::env::temp_dir().join(format!("hexcleaner_themes_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("ocean.json"), r##"{"name": "Ocean", "base": "light", "colors": {"accent": "#0077BE"}}"##).unwrap();
        // Без названия и основы: название — имя файла, основа — тёмная.
        fs::write(dir.join("plain.json"), "\u{feff}{}").unwrap();
        fs::write(dir.join("dark.json"), r##"{"colors": {"accent": "#FF0000"}, "chart": ["#000000"]}"##).unwrap();
        fs::write(dir.join("broken.json"), "{ not json").unwrap();

        let (themes, broken) = available_in(&dir);
        let ids: Vec<&str> = themes.iter().map(|theme| theme.id.as_str()).collect();
        for id in ["dark", "light", "ocean", "plain"] {
            assert!(ids.contains(&id), "нет темы {id}: {ids:?}");
        }
        assert!(themes.iter().any(|theme| theme.id == "ocean" && theme.name == "Ocean"));
        assert!(themes.iter().any(|theme| theme.id == "plain" && theme.name == "plain"));
        assert_eq!(broken, ["broken.json"]);

        let ocean = load_from("ocean", &dir).unwrap();
        assert_eq!(ocean.base(), Base::Light);
        assert_eq!(ocean.colors["accent"], "#0077BE");
        assert_eq!(load_from("plain", &dir).unwrap().base(), Base::Dark);

        // Свой dark.json меняет только то, что в нём есть.
        let dark = load_from("dark", &dir).unwrap();
        let built_in_dark = built_in("dark").unwrap();
        assert_eq!(dark.colors["accent"], "#FF0000");
        assert_eq!(dark.colors.len(), built_in_dark.colors.len());
        assert_eq!(dark.chart, ["#000000"]);
        assert_eq!(dark.base(), Base::Dark);
        assert_eq!(dark.name, built_in_dark.name);

        assert!(matches!(load_from("no_such_theme", &dir), Err(ThemeError::NotFound)));
        assert!(matches!(load_from("broken", &dir), Err(ThemeError::Invalid { .. })));
        let _ = fs::remove_dir_all(&dir);
    }

}
