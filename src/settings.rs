//! Сохранение выбранных пунктов, наборов галочек, языка, темы и других настроек между запусками.
//!
//! Настройки хранятся в `%APPDATA%\HexCleaner\settings.json`. Благодаря этому выбор
//! пунктов, требующих прав администратора, не теряется при запуске без прав
//! и при перезапуске программы от имени администратора.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::catalog::CleanupTask;
use crate::presets::{Selection, UserPreset};
use crate::schedule::Schedule;
use crate::trash::TrashSettings;
use crate::update::UpdateSettings;

/// Как сжимать: корзину или журналы прошлых очисток.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// Сжатие файловой системы NTFS: файлы и папки остаются как есть.
    #[default]
    Ntfs,
    /// Упаковка в zip-архивы.
    Zip,
}

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
    /// Сжимать ли журналы прошлых очисток.
    pub compress_old_logs: bool,
    /// Как: каждый журнал в свой zip-архив или сжатием NTFS всей папки журналов.
    pub log_compression: Compression,
    /// Код языка (имя файла перевода). `None` — язык Windows.
    pub language: Option<String>,
    pub theme: Theme,
    /// Всплывающее уведомление, когда анализ или очистка закончились.
    pub notify_when_done: bool,
    /// Мигать кнопкой на панели задач, если окно неактивно, когда анализ или очистка закончились.
    pub flash_taskbar: bool,
    pub updates: UpdateSettings,
    pub trash: TrashSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            selected: BTreeMap::new(),
            presets: Vec::new(),
            schedules: Vec::new(),
            compress_old_logs: true,
            log_compression: Compression::Zip,
            language: None,
            theme: Theme::default(),
            notify_when_done: true,
            flash_taskbar: true,
            updates: UpdateSettings::default(),
            trash: TrashSettings::default(),
        }
    }
}

impl Settings {
    /// Загружает настройки; если файла нет или он повреждён — настройки по умолчанию.
    pub fn load() -> Self {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = self.try_save();
    }

    /// Как [`Settings::save`], но с ошибкой — для командной строки, где её есть кому показать.
    pub fn try_save(&self) -> io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(path, text)
    }

    /// Файл настроек: `%APPDATA%\HexCleaner\settings.json`.
    pub fn path() -> PathBuf {
        let app_data = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_owned());
        PathBuf::from(app_data).join("HexCleaner").join("settings.json")
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
