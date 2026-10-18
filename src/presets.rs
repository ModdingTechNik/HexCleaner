//! Наборы галочек.
//!
//! Встроенные наборы строятся по уровням безопасности пунктов ([`Safety`]):
//! «Только безопасное», «Рекомендуемый» (безопасное и умеренное) и «Полный».
//! Свои наборы пользователь сохраняет сам — они хранятся в настройках.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::catalog::{CleanupTask, Safety};

/// Отмеченные пункты (их `CleanupTask::id`).
pub type Selection = BTreeSet<String>;

/// Набор, сохранённый пользователем.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPreset {
    pub name: String,
    pub tasks: Selection,
}

/// Какой набор соответствует текущим галочкам.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivePreset {
    /// Встроенный набор: все пункты не опаснее этого уровня.
    BuiltIn(Safety),
    User(String),
    /// Галочки не совпадают ни с одним набором.
    Custom,
}

/// Пункты встроенного набора — все, что не опаснее `max_safety`.
pub fn built_in(tasks: &[CleanupTask], max_safety: Safety) -> Selection {
    tasks.iter().filter(|task| task.safety <= max_safety).map(|task| task.id.to_owned()).collect()
}

/// Набор, совпадающий с `selection`. Встроенные наборы проверяются первыми.
pub fn find_active(tasks: &[CleanupTask], presets: &[UserPreset], selection: &Selection) -> ActivePreset {
    if let Some(level) = Safety::ALL.into_iter().find(|&level| built_in(tasks, level) == *selection) {
        return ActivePreset::BuiltIn(level);
    }
    match presets.iter().find(|preset| preset.tasks == *selection) {
        Some(preset) => ActivePreset::User(preset.name.clone()),
        None => ActivePreset::Custom,
    }
}

/// Сохраняет набор. Набор с таким же названием перезаписывается.
pub fn save(presets: &mut Vec<UserPreset>, name: &str, tasks: Selection) {
    let name = name.trim();
    match presets.iter_mut().find(|preset| preset.name == name) {
        Some(preset) => preset.tasks = tasks,
        None => presets.push(UserPreset { name: name.to_owned(), tasks }),
    }
}

pub fn delete(presets: &mut Vec<UserPreset>, name: &str) {
    presets.retain(|preset| preset.name != name);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::all_tasks;

    fn selection(ids: &[&str]) -> Selection {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn built_in_presets_follow_safety_levels() {
        let tasks = all_tasks();
        let safe = built_in(&tasks, Safety::Safe);
        let recommended = built_in(&tasks, Safety::Moderate);
        let full = built_in(&tasks, Safety::Caution);

        assert!(safe.contains("user_temp") && safe.contains("browsers_cache"));
        assert!(!safe.contains("windows_update_cache"));
        assert!(recommended.is_superset(&safe) && recommended.contains("windows_update_cache"));
        assert!(!recommended.contains("system_logs"));
        assert_eq!(full.len(), tasks.len());
    }

    #[test]
    fn default_selection_is_the_recommended_preset() {
        let tasks = all_tasks();
        let defaults: Selection = tasks
            .iter()
            .filter(|task| task.safety.selected_by_default())
            .map(|task| task.id.to_owned())
            .collect();
        assert_eq!(find_active(&tasks, &[], &defaults), ActivePreset::BuiltIn(Safety::Moderate));
    }

    #[test]
    fn user_presets_are_saved_matched_and_deleted() {
        let tasks = all_tasks();
        let mut presets = Vec::new();

        save(&mut presets, "  Браузеры  ", selection(&["browsers_cache"]));
        assert_eq!(presets[0].name, "Браузеры");
        assert_eq!(
            find_active(&tasks, &presets, &selection(&["browsers_cache"])),
            ActivePreset::User("Браузеры".to_owned())
        );
        assert_eq!(find_active(&tasks, &presets, &selection(&["user_temp"])), ActivePreset::Custom);

        // Повторное сохранение под тем же именем заменяет набор, а не добавляет второй.
        save(&mut presets, "Браузеры", selection(&["browsers_cache", "user_temp"]));
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].tasks, selection(&["browsers_cache", "user_temp"]));

        delete(&mut presets, "Браузеры");
        assert!(presets.is_empty());
    }
}
