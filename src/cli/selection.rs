//! Какие пункты обработать: наборы, отдельные пункты и исключения из командной строки
//! превращаются в список пунктов каталога.

use crate::catalog::CleanupTask;
use crate::presets::{self, Selection};
use crate::settings::Settings;

use super::UsageError;
use super::args::TaskChoice;

/// Индексы выбранных пунктов в каталоге — в том порядке, в котором пункты выполняются.
pub fn resolve(choice: &TaskChoice, tasks: &[CleanupTask], settings: &Settings) -> Result<Vec<usize>, UsageError> {
    let mut selection = Selection::new();

    if choice.selected || choice.is_empty() {
        selection.extend(settings.selection(tasks));
    }
    for name in &choice.presets {
        selection.extend(preset(name, tasks, settings)?);
    }
    for id in &choice.tasks {
        selection.insert(task_id(id, tasks)?);
    }
    for id in &choice.exclude {
        selection.remove(&task_id(id, tasks)?);
    }

    let indices: Vec<usize> = (0..tasks.len()).filter(|&index| selection.contains(tasks[index].id)).collect();
    if indices.is_empty() {
        return Err(UsageError::NothingSelected);
    }
    Ok(indices)
}

/// Встроенный набор (`safe`, `recommended`, `full`) или свой — по названию.
/// Регистр букв не важен, но точное совпадение названия своего набора важнее.
fn preset(name: &str, tasks: &[CleanupTask], settings: &Settings) -> Result<Selection, UsageError> {
    if let Some(level) = presets::built_in_by_id(name) {
        return Ok(presets::built_in(tasks, level));
    }
    let user_presets = &settings.presets;
    user_presets
        .iter()
        .find(|preset| preset.name == name)
        .or_else(|| user_presets.iter().find(|preset| preset.name.to_lowercase() == name.to_lowercase()))
        .map(|preset| preset.tasks.clone())
        .ok_or_else(|| UsageError::UnknownPreset(name.to_owned()))
}

/// Проверяет, что такой пункт есть в каталоге (регистр не важен), и возвращает его точный id.
fn task_id(id: &str, tasks: &[CleanupTask]) -> Result<String, UsageError> {
    tasks
        .iter()
        .find(|task| task.id.eq_ignore_ascii_case(id))
        .map(|task| task.id.to_owned())
        .ok_or_else(|| UsageError::UnknownTask(id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Safety, all_tasks};

    fn ids(indices: &[usize], tasks: &[CleanupTask]) -> Vec<&'static str> {
        indices.iter().map(|&index| tasks[index].id).collect()
    }

    fn choice(presets: &[&str], tasks: &[&str], exclude: &[&str]) -> TaskChoice {
        let owned = |items: &[&str]| items.iter().map(|item| item.to_string()).collect();
        TaskChoice { presets: owned(presets), tasks: owned(tasks), exclude: owned(exclude), selected: false }
    }

    #[test]
    fn combines_presets_tasks_and_exclusions_in_catalog_order() {
        let tasks = all_tasks();
        let settings = Settings::default();

        let result = resolve(&choice(&["SAFE"], &["recycle_bin"], &["browsers_cache"]), &tasks, &settings).unwrap();
        let result = ids(&result, &tasks);

        assert!(result.contains(&"user_temp") && result.contains(&"recycle_bin"));
        assert!(!result.contains(&"browsers_cache"));
        assert!(!result.contains(&"system_logs"));
        // Порядок — как в каталоге, а не как в командной строке.
        assert_eq!(result.last(), Some(&"recycle_bin"));
    }

    #[test]
    fn without_choice_uses_selection_from_the_program() {
        let tasks = all_tasks();
        let mut settings = Settings::default();
        // Пользователь ничего не трогал — берётся рекомендуемый набор.
        let result = resolve(&TaskChoice::default(), &tasks, &settings).unwrap();
        assert_eq!(ids(&result, &tasks).len(), presets::built_in(&tasks, Safety::Moderate).len());

        for task in &tasks {
            settings.set_selected(task.id, task.id == "user_temp");
        }
        let result = resolve(&TaskChoice::default(), &tasks, &settings).unwrap();
        assert_eq!(ids(&result, &tasks), ["user_temp"]);
    }

    #[test]
    fn finds_user_presets_by_name() {
        let tasks = all_tasks();
        let mut settings = Settings::default();
        presets::save(&mut settings.presets, "Раз в неделю", ["recycle_bin".to_owned()].into());

        let result = resolve(&choice(&["раз в неделю"], &[], &[]), &tasks, &settings).unwrap();
        assert_eq!(ids(&result, &tasks), ["recycle_bin"]);
    }

    #[test]
    fn reports_unknown_names_and_empty_selection() {
        let tasks = all_tasks();
        let settings = Settings::default();
        let error = |choice| resolve(&choice, &tasks, &settings).unwrap_err();

        assert_eq!(error(choice(&["nightly"], &[], &[])), UsageError::UnknownPreset("nightly".into()));
        assert_eq!(error(choice(&[], &["cookies"], &[])), UsageError::UnknownTask("cookies".into()));
        assert_eq!(error(choice(&[], &["user_temp"], &["user_temp"])), UsageError::NothingSelected);
    }
}
