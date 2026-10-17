//! Сохранение выбранных пунктов между запусками.
//!
//! Настройки хранятся в `%APPDATA%\HexCleaner\settings.json`. Благодаря этому выбор
//! пунктов, требующих прав администратора, не теряется при запуске без прав
//! и при перезапуске программы от имени администратора.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Отмечен ли пункт (ключ — `CleanupTask::id`). Пункты, которых здесь нет, отмечены.
    selected: BTreeMap<String, bool>,
    /// Сжимать ли журналы прошлых очисток в zip-архивы.
    pub compress_old_logs: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { selected: BTreeMap::new(), compress_old_logs: true }
    }
}

impl Settings {
    /// Загружает настройки; если файла нет или он повреждён — настройки по умолчанию.
    pub fn load() -> Self {
        fs::read_to_string(settings_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, text);
        }
    }

    pub fn is_selected(&self, task_id: &str) -> bool {
        self.selected.get(task_id).copied().unwrap_or(true)
    }

    pub fn set_selected(&mut self, task_id: &str, selected: bool) {
        self.selected.insert(task_id.to_owned(), selected);
    }
}

fn settings_path() -> PathBuf {
    let app_data = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(app_data).join("HexCleaner").join("settings.json")
}
