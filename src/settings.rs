//! Сохранение выбранных пунктов, наборов галочек, языка, темы и других настроек между запусками.
//!
//! Настройки хранятся в `%APPDATA%\HexCleaner\settings.json`. Благодаря этому выбор
//! пунктов, требующих прав администратора, не теряется при запуске без прав
//! и при перезапуске программы от имени администратора.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::catalog::CleanupTask;
use crate::presets::{Selection, UserPreset};
use crate::schedule::Schedule;
use crate::update::UpdateSettings;

/// Тема оформления.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    /// Как в настройках Windows.
    System,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Отмечен ли пункт (ключ — `CleanupTask::id`). Пунктов, которых пользователь
    /// ещё не трогал, здесь нет — для них действует выбор по умолчанию.
    selected: BTreeMap<String, bool>,
    /// Наборы галочек, сохранённые пользователем.
    pub presets: Vec<UserPreset>,
    /// Задания плановой очистки. Сами задания живут в Планировщике заданий Windows,
    /// здесь — их описание, по которому их можно создать заново.
    pub schedules: Vec<Schedule>,
    /// Сжимать ли журналы прошлых очисток в zip-архивы.
    pub compress_old_logs: bool,
    /// Код языка (имя файла перевода). `None` — язык Windows.
    pub language: Option<String>,
    pub theme: Theme,
    /// Всплывающее уведомление, когда анализ или очистка закончились.
    pub notify_when_done: bool,
    /// Мигать кнопкой на панели задач, если окно неактивно, когда анализ или очистка закончились.
    pub flash_taskbar: bool,
    pub updates: UpdateSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            selected: BTreeMap::new(),
            presets: Vec::new(),
            schedules: Vec::new(),
            compress_old_logs: true,
            language: None,
            theme: Theme::default(),
            notify_when_done: true,
            flash_taskbar: true,
            updates: UpdateSettings::default(),
        }
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

    /// Отмечен ли пункт; `None` — пользователь его ещё не трогал.
    pub fn is_selected(&self, task_id: &str) -> Option<bool> {
        self.selected.get(task_id).copied()
    }

    pub fn set_selected(&mut self, task_id: &str, selected: bool) {
        self.selected.insert(task_id.to_owned(), selected);
    }

    /// Отмечен ли пункт с учётом выбора по умолчанию: пункты, которые пользователь
    /// ещё не трогал, отмечены, если они входят в рекомендуемый набор.
    pub fn is_task_selected(&self, task: &CleanupTask) -> bool {
        self.is_selected(task.id).unwrap_or(task.safety.selected_by_default())
    }

    /// Все отмеченные пункты, включая требующие прав администратора.
    pub fn selection(&self, tasks: &[CleanupTask]) -> Selection {
        tasks.iter().filter(|task| self.is_task_selected(task)).map(|task| task.id.to_owned()).collect()
    }
}

fn settings_path() -> PathBuf {
    let app_data = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(app_data).join("HexCleaner").join("settings.json")
}
