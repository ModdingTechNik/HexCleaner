//! Какие пункты обработать: наборы, отдельные пункты и исключения из командной строки
//! превращаются в список пунктов каталога.

use crate::catalog::CleanupTask;
use crate::presets::{self, Selection};
use crate::settings::Settings;

use super::UsageError;
use super::args::TaskChoice;

/// Что выбрано.
#[derive(Debug, Default, PartialEq)]
pub struct Resolved {
    /// Индексы выбранных пунктов в каталоге — в том порядке, в котором пункты выполняются.
    /// Может быть пустым: о том, что выбирать нечего, сообщает вызывающий.
    pub indices: Vec<usize>,
    /// Пункты из `--tasks` и `--exclude`, которых нет в каталоге: опечатка или удалённый
    /// свой пункт, который ещё остался в задании планировщика. Они пропускаются.
    pub missing: Vec<String>,
}

/// Пункты по наборам, `--tasks`, `--exclude` и `--selected`. Неизвестный набор — ошибка,
/// неизвестный пункт — нет (см. [`Resolved::missing`]): иначе удаление одного своего пункта
/// остановило бы все задания планировщика, где он был.
pub fn resolve(choice: &TaskChoice, tasks: &[CleanupTask], settings: &Settings) -> Result<Resolved, UsageError> {
    let mut selection = Selection::new();
    let mut missing = Vec::new();

    if choice.selected || choice.is_empty() {
        selection.extend(settings.selection(tasks));
    }
    for name in &choice.presets {
        selection.extend(preset(name, tasks, settings)?);
    }
    for id in &choice.tasks {
        match task_id(id, tasks) {
            Some(id) => selection.insert(id),
            None => {
                missing.push(id.clone());
                false
            }
        };
    }
    for id in &choice.exclude {
        match task_id(id, tasks) {
            Some(id) => selection.remove(&id),
            None => {
                missing.push(id.clone());
                false
            }
        };
    }

    let indices = (0..tasks.len()).filter(|&index| selection.contains(tasks[index].id)).collect();
    Ok(Resolved { indices, missing })
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

/// Точный id пункта из каталога (регистр не важен); `None` — такого пункта нет.
fn task_id(id: &str, tasks: &[CleanupTask]) -> Option<String> {
    tasks.iter().find(|task| task.id.eq_ignore_ascii_case(id)).map(|task| task.id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Safety, all_tasks};

    fn ids(resolved: &Resolved, tasks: &[CleanupTask]) -> Vec<&'static str> {
        resolved.indices.iter().map(|&index| tasks[index].id).collect()
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
    fn unknown_preset_is_an_error_unknown_tasks_are_skipped() {
        let tasks = all_tasks();
        let settings = Settings::default();

        let error = resolve(&choice(&["nightly"], &[], &[]), &tasks, &settings).unwrap_err();
        assert_eq!(error, UsageError::UnknownPreset("nightly".into()));

        // Удалённый свой пункт в задании: остальные пункты выполняются.
        let resolved = resolve(&choice(&[], &["user_temp", "deleted_task"], &["gone"]), &tasks, &settings).unwrap();
        assert_eq!(ids(&resolved, &tasks), ["user_temp"]);
        assert_eq!(resolved.missing, ["deleted_task", "gone"]);

        let resolved = resolve(&choice(&[], &["user_temp"], &["user_temp"]), &tasks, &settings).unwrap();
        assert!(resolved.indices.is_empty() && resolved.missing.is_empty());
    }

    /// Свой набор, в котором остался удалённый пункт, работает без него.
    #[test]
    fn user_preset_with_deleted_task_still_works() {
        let tasks = all_tasks();
        let mut settings = Settings::default();
        presets::save(&mut settings.presets, "Старый", ["recycle_bin".to_owned(), "deleted_task".to_owned()].into());

        let resolved = resolve(&choice(&["Старый"], &[], &[]), &tasks, &settings).unwrap();
        assert_eq!(ids(&resolved, &tasks), ["recycle_bin"]);
        assert!(resolved.missing.is_empty());
    }
}
