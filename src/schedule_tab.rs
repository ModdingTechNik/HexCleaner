//! Вкладка «Плановая очистка»: задания в Планировщике заданий Windows.
//!
//! Описание заданий хранится в настройках программы, а Планировщик — источник правды
//! о том, что задание существует, включено и когда запускалось. Вкладка регулярно сверяет
//! одно с другим: если задание удалили в «Планировщике заданий», оно остаётся в списке
//! с пометкой и предложением создать его снова или убрать из списка.

use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDateTime};
use eframe::egui::{self, Button, Checkbox, ComboBox, DragValue, Frame, Id, Layout, Modal, RichText, ScrollArea, TextEdit, Ui};
use hexcleaner::catalog::{CleanupTask, Safety};
use hexcleaner::presets::{self, Selection};
use hexcleaner::schedule::{Repeat, Schedule, weekday_short_name};
use hexcleaner::settings::Settings;
use hexcleaner::system::task_scheduler::{Scheduler, TaskInfo};
use hexcleaner::t;

use crate::widgets::{shield_atom, shield_icon};

/// Как часто сверяться с Планировщиком, пока вкладка открыта.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Коды возврата командной строки (см. `hexcleaner::cli::ExitCode`) и Планировщика.
const EXIT_SUCCESS: i32 = 0;
const EXIT_BAD_ARGUMENTS: i32 = 1;
const EXIT_PARTIALLY_DONE: i32 = 2;
const EXIT_FAILED: i32 = 3;
const EXIT_CANCELLED: i32 = 4;
/// Задание выполняется прямо сейчас (`SCHED_S_TASK_RUNNING`).
const TASK_RUNNING: i32 = 0x0004_1301;

#[derive(Default)]
pub struct ScheduleTab {
    /// Задания из папки HexCleaner Планировщика по последней проверке.
    found: Vec<TaskInfo>,
    last_refresh: Option<Instant>,
    /// Ошибка последнего обращения к Планировщику.
    error: Option<String>,
    editor: Option<Editor>,
    /// Название задания, удаление которого ждёт подтверждения.
    confirm_delete: Option<String>,
}

/// Окно создания или изменения задания.
struct Editor {
    /// Название до правки; `None` — новое задание.
    original_name: Option<String>,
    draft: Schedule,
    error: Option<String>,
}

/// Что сделать после отрисовки (действия меняют список, по которому идёт отрисовка).
enum Action {
    New,
    Edit(usize),
    AskDelete(usize),
    SetEnabled(usize, bool),
    RunNow(usize),
    Recreate(usize),
    Forget(usize),
    DeleteFound(String),
}

impl ScheduleTab {
    pub fn ui(&mut self, ui: &mut Ui, settings: &mut Settings, tasks: &[CleanupTask], is_admin: bool) {
        if self.last_refresh.is_none_or(|time| time.elapsed() >= REFRESH_INTERVAL) {
            self.refresh();
        }
        // Пока вкладка открыта, сверяемся с Планировщиком: задания могли изменить или удалить там.
        ui.ctx().request_repaint_after(REFRESH_INTERVAL);

        let mut action = None;
        ui.horizontal(|ui| {
            if ui.button(RichText::new(t!("schedule.add")).strong()).clicked() {
                action = Some(Action::New);
            }
            if ui.button(t!("schedule.refresh")).clicked() {
                self.refresh();
            }
            if ui.button(t!("schedule.open_task_scheduler")).clicked() {
                open_task_scheduler();
            }
        });
        ui.label(RichText::new(t!("schedule.hint")).weak());
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
        }
        ui.separator();

        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if settings.schedules.is_empty() {
                ui.label(RichText::new(t!("schedule.empty")).weak());
            }
            for (index, schedule) in settings.schedules.iter().enumerate() {
                let status = self.found.iter().find(|task| same_name(&task.name, &schedule.name));
                if let Some(chosen) = schedule_card(ui, schedule, status, tasks) {
                    action = Some(match chosen {
                        CardAction::Edit => Action::Edit(index),
                        CardAction::Delete => Action::AskDelete(index),
                        CardAction::SetEnabled(enabled) => Action::SetEnabled(index, enabled),
                        CardAction::RunNow => Action::RunNow(index),
                        CardAction::Recreate => Action::Recreate(index),
                        CardAction::Forget => Action::Forget(index),
                    });
                }
            }

            // Задания HexCleaner в Планировщике, которых нет в списке (например, после переустановки).
            let unknown: Vec<&TaskInfo> = self
                .found
                .iter()
                .filter(|task| !settings.schedules.iter().any(|schedule| same_name(&task.name, &schedule.name)))
                .collect();
            if !unknown.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new(t!("schedule.unknown_title")).strong());
                ui.label(RichText::new(t!("schedule.unknown_hint")).weak());
                for task in unknown {
                    ui.horizontal(|ui| {
                        ui.label(&task.name);
                        ui.label(RichText::new(run_status(task)).weak());
                        if ui.small_button(t!("schedule.delete_from_scheduler")).clicked() {
                            action = Some(Action::DeleteFound(task.name.clone()));
                        }
                    });
                }
            }
        });

        if let Some(action) = action {
            self.apply(action, settings, tasks);
        }
        self.editor_window(ui.ctx(), settings, tasks, is_admin);
        self.delete_confirmation(ui.ctx(), settings);
    }

    /// Перечитывает задания из Планировщика.
    fn refresh(&mut self) {
        self.last_refresh = Some(Instant::now());
        match Scheduler::connect().and_then(|scheduler| scheduler.tasks()) {
            Ok(found) => {
                self.found = found;
                self.error = None;
            }
            Err(error) => self.error = Some(t!("schedule.error.scheduler", error = error.message())),
        }
    }

    /// Выполняет действие в Планировщике; при ошибке показывает её. Затем перечитывает задания.
    fn in_scheduler(&mut self, action: impl FnOnce(&Scheduler) -> windows::core::Result<()>) -> bool {
        let result = Scheduler::connect().and_then(|scheduler| action(&scheduler));
        self.refresh();
        match result {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(t!("schedule.error.scheduler", error = error.message()));
                false
            }
        }
    }

    fn apply(&mut self, action: Action, settings: &mut Settings, tasks: &[CleanupTask]) {
        match action {
            Action::New => {
                let selection = ordered(tasks, &settings.selection(tasks));
                let name = unique_name(settings);
                let draft = Schedule::new(name, selection, t!("schedule.tasks.selected_in_program"));
                self.editor = Some(Editor { original_name: None, draft, error: None });
            }
            Action::Edit(index) => {
                let draft = settings.schedules[index].clone();
                self.editor = Some(Editor { original_name: Some(draft.name.clone()), draft, error: None });
            }
            Action::AskDelete(index) => self.confirm_delete = Some(settings.schedules[index].name.clone()),
            Action::SetEnabled(index, enabled) => {
                let name = settings.schedules[index].name.clone();
                if self.in_scheduler(|scheduler| scheduler.set_enabled(&name, enabled)) {
                    settings.schedules[index].enabled = enabled;
                    settings.save();
                }
            }
            Action::RunNow(index) => {
                let name = settings.schedules[index].name.clone();
                self.in_scheduler(|scheduler| scheduler.run_now(&name));
            }
            Action::Recreate(index) => {
                let schedule = settings.schedules[index].clone();
                self.in_scheduler(|scheduler| register(scheduler, &schedule));
            }
            Action::Forget(index) => {
                settings.schedules.remove(index);
                settings.save();
            }
            Action::DeleteFound(name) => {
                self.in_scheduler(|scheduler| scheduler.delete(&name));
            }
        }
    }

    /// Окно создания или изменения задания.
    fn editor_window(&mut self, ctx: &egui::Context, settings: &mut Settings, tasks: &[CleanupTask], is_admin: bool) {
        let Some(editor) = &mut self.editor else { return };
        let mut save = false;
        let mut cancel = false;

        let modal = Modal::new(Id::unique("schedule_editor")).show(ctx, |ui| {
            ui.set_width(480.0);
            let title = if editor.original_name.is_some() { t!("schedule.edit_title") } else { t!("schedule.new_title") };
            ui.heading(title);
            ui.add_space(6.0);
            draft_form(ui, &mut editor.draft, settings, tasks, is_admin);

            if let Some(error) = &editor.error {
                ui.add_space(4.0);
                ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
            }
            ui.separator();
            ui.horizontal(|ui| {
                save = ui.button(RichText::new(t!("schedule.save")).strong()).clicked();
                cancel = ui.button(t!("schedule.cancel")).clicked();
            });
        });
        if modal.should_close() {
            cancel = true;
        }

        if cancel {
            self.editor = None;
        } else if save {
            self.save_editor(settings);
        }
    }

    /// Проверяет задание, регистрирует его в Планировщике и запоминает в настройках.
    fn save_editor(&mut self, settings: &mut Settings) {
        let Some(editor) = &mut self.editor else { return };
        editor.draft.name = editor.draft.name.trim().to_owned();
        let original = editor.original_name.clone();
        let others = settings
            .schedules
            .iter()
            .filter(|schedule| original.as_deref().is_none_or(|name| !same_name(&schedule.name, name)))
            .map(|schedule| schedule.name.as_str());
        if let Err(error) = editor.draft.validate(others) {
            editor.error = Some(error.message());
            return;
        }

        let draft = editor.draft.clone();
        let renamed_from = original.clone().filter(|name| !same_name(name, &draft.name));
        let saved = self.in_scheduler(|scheduler| {
            register(scheduler, &draft)?;
            match &renamed_from {
                Some(old_name) => scheduler.delete(old_name),
                None => Ok(()),
            }
        });
        if !saved {
            if let Some(editor) = &mut self.editor {
                editor.error = self.error.clone();
            }
            return;
        }

        match original.and_then(|name| settings.schedules.iter().position(|schedule| same_name(&schedule.name, &name))) {
            Some(index) => settings.schedules[index] = draft,
            None => settings.schedules.push(draft),
        }
        settings.save();
        self.editor = None;
    }

    fn delete_confirmation(&mut self, ctx: &egui::Context, settings: &mut Settings) {
        let Some(name) = self.confirm_delete.clone() else { return };
        let mut delete = false;
        let mut cancel = false;

        let modal = Modal::new(Id::unique("schedule_delete")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(t!("schedule.delete_question", name = name));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                delete = ui.button(RichText::new(t!("schedule.delete")).strong()).clicked();
                cancel = ui.button(t!("schedule.cancel")).clicked();
            });
        });
        if modal.should_close() {
            cancel = true;
        }

        if delete {
            self.confirm_delete = None;
            if self.in_scheduler(|scheduler| scheduler.delete(&name)) {
                settings.schedules.retain(|schedule| !same_name(&schedule.name, &name));
                settings.save();
            }
        } else if cancel {
            self.confirm_delete = None;
        }
    }
}

/// Что выбрали в карточке задания.
enum CardAction {
    Edit,
    Delete,
    SetEnabled(bool),
    RunNow,
    Recreate,
    Forget,
}

/// Карточка задания: расписание, что чистит и его состояние в Планировщике.
fn schedule_card(ui: &mut Ui, schedule: &Schedule, status: Option<&TaskInfo>, tasks: &[CleanupTask]) -> Option<CardAction> {
    let mut action = None;
    Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());

        ui.horizontal(|ui| {
            let mut enabled = status.is_some_and(|task| task.enabled);
            let checkbox = ui
                .add_enabled(status.is_some(), Checkbox::new(&mut enabled, RichText::new(&schedule.name).strong()))
                .on_hover_text(t!("schedule.enabled_hint"));
            if checkbox.changed() {
                action = Some(CardAction::SetEnabled(enabled));
            }
            if schedule.run_as_admin {
                shield_icon(ui, 14.0).on_hover_text(t!("schedule.run_as_admin"));
            }

            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t!("schedule.delete")).clicked() {
                    action = Some(CardAction::Delete);
                }
                if ui.button(t!("schedule.edit")).clicked() {
                    action = Some(CardAction::Edit);
                }
                if status.is_some() && ui.button(t!("schedule.run_now")).clicked() {
                    action = Some(CardAction::RunNow);
                }
            });
        });

        let mut details = vec![
            schedule.describe_repeat(),
            t!("schedule.tasks_summary", label = schedule.tasks_label, count = schedule.tasks.len()),
        ];
        if schedule.close_apps {
            details.push(t!("schedule.closes_apps"));
        }
        if schedule.no_trash {
            details.push(t!("schedule.bypasses_trash"));
        }
        ui.label(RichText::new(details.join(" · ")).weak()).on_hover_text(task_titles(tasks, &schedule.tasks));

        match status {
            Some(task) => {
                ui.label(run_status(task));
            }
            None => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(t!("schedule.missing")).color(ui.visuals().warn_fg_color));
                    if ui.small_button(t!("schedule.recreate")).clicked() {
                        action = Some(CardAction::Recreate);
                    }
                    if ui.small_button(t!("schedule.forget")).clicked() {
                        action = Some(CardAction::Forget);
                    }
                });
            }
        }
    });
    ui.add_space(4.0);
    action
}

/// Поля задания в окне редактирования.
fn draft_form(ui: &mut Ui, draft: &mut Schedule, settings: &Settings, tasks: &[CleanupTask], is_admin: bool) {
    egui::Grid::new("schedule_form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(t!("schedule.field.name"));
        ui.add(TextEdit::singleline(&mut draft.name).desired_width(280.0));
        ui.end_row();

        ui.label(t!("schedule.field.tasks"));
        tasks_selector(ui, draft, settings, tasks);
        ui.end_row();

        ui.label(t!("schedule.field.repeat"));
        ui.horizontal(|ui| {
            let is_daily = matches!(draft.repeat, Repeat::Daily);
            let is_weekly = matches!(draft.repeat, Repeat::Weekly { .. });
            let is_monthly = matches!(draft.repeat, Repeat::Monthly { .. });
            if ui.radio(is_daily, t!("schedule.repeat.daily")).clicked() {
                draft.repeat = Repeat::Daily;
            }
            if ui.radio(is_weekly, t!("schedule.repeat.weekly")).clicked() && !is_weekly {
                let mut days = [false; 7];
                days[6] = true;
                draft.repeat = Repeat::Weekly { days };
            }
            if ui.radio(is_monthly, t!("schedule.repeat.monthly")).clicked() && !is_monthly {
                draft.repeat = Repeat::Monthly { day: 1 };
            }
        });
        ui.end_row();

        match &mut draft.repeat {
            Repeat::Daily => {}
            Repeat::Weekly { days } => {
                ui.label(t!("schedule.field.weekdays"));
                ui.horizontal(|ui| {
                    for (day, selected) in days.iter_mut().enumerate() {
                        ui.toggle_value(selected, weekday_short_name(day));
                    }
                });
                ui.end_row();
            }
            Repeat::Monthly { day } => {
                ui.label(t!("schedule.field.day_of_month"));
                ui.add(DragValue::new(day).range(1..=28)).on_hover_text(t!("schedule.day_of_month_hint"));
                ui.end_row();
            }
        }

        ui.label(t!("schedule.field.time"));
        ui.horizontal(|ui| {
            ui.add(DragValue::new(&mut draft.hour).range(0..=23).custom_formatter(|value, _| format!("{value:02}")));
            ui.label(":");
            ui.add(DragValue::new(&mut draft.minute).range(0..=59).custom_formatter(|value, _| format!("{value:02}")));
        });
        ui.end_row();
    });

    ui.add_space(6.0);
    ui.checkbox(&mut draft.close_apps, t!("schedule.field.close_apps")).on_hover_text(t!("schedule.close_apps_hint"));
    ui.checkbox(&mut draft.no_trash, t!("schedule.field.no_trash")).on_hover_text(t!("schedule.no_trash_hint"));

    let admin_checkbox = Checkbox::new(&mut draft.run_as_admin, (shield_atom(14.0), t!("schedule.run_as_admin")));
    ui.add_enabled(is_admin, admin_checkbox)
        .on_hover_text(t!("schedule.run_as_admin_hint"))
        .on_disabled_hover_text(t!("schedule.run_as_admin_needs_admin"));

    let needs_admin = tasks.iter().any(|task| task.requires_admin && draft.tasks.iter().any(|id| id == task.id));
    if needs_admin && !draft.run_as_admin {
        ui.label(RichText::new(t!("schedule.admin_tasks_skipped")).color(ui.visuals().warn_fg_color));
    }
}

/// Выбор пунктов: встроенный набор, свой набор или пункты, отмеченные сейчас в программе.
fn tasks_selector(ui: &mut Ui, draft: &mut Schedule, settings: &Settings, tasks: &[CleanupTask]) {
    let mut options: Vec<(String, Vec<String>)> = Safety::ALL
        .into_iter()
        .map(|level| (presets::built_in_name(level), ordered(tasks, &presets::built_in(tasks, level))))
        .collect();
    options.extend(settings.presets.iter().map(|preset| (preset.name.clone(), ordered(tasks, &preset.tasks))));
    options.push((t!("schedule.tasks.selected_in_program"), ordered(tasks, &settings.selection(tasks))));

    let selected_text = t!("schedule.tasks_summary", label = draft.tasks_label, count = draft.tasks.len());
    ComboBox::from_id_salt("schedule_tasks")
        .selected_text(selected_text)
        .width(280.0)
        .show_ui(ui, |ui| {
            for (label, ids) in options {
                let text = t!("schedule.tasks_summary", label = label, count = ids.len());
                let selected = draft.tasks == ids;
                if ui.add(Button::selectable(selected, text)).on_hover_text(task_titles(tasks, &ids)).clicked() {
                    draft.tasks = ids;
                    draft.tasks_label = label;
                }
            }
        })
        .response
        .on_hover_text(task_titles(tasks, &draft.tasks));
}

/// Состояние задания в Планировщике одной строкой.
fn run_status(task: &TaskInfo) -> String {
    let next = if task.running {
        t!("schedule.status.running")
    } else if !task.enabled {
        t!("schedule.status.disabled")
    } else {
        match task.next_run {
            Some(time) => t!("schedule.status.next_run", time = format_time(time)),
            None => t!("schedule.status.no_next_run"),
        }
    };
    let last = match (task.last_run, task.last_result) {
        (Some(time), Some(code)) => {
            t!("schedule.status.last_run", time = format_time(time), result = result_text(code))
        }
        _ => t!("schedule.status.never_run"),
    };
    format!("{next} · {last}")
}

/// Что означает код возврата последнего запуска.
fn result_text(code: i32) -> String {
    match code {
        EXIT_SUCCESS => t!("schedule.result.success"),
        EXIT_PARTIALLY_DONE => t!("schedule.result.partially_done"),
        EXIT_BAD_ARGUMENTS => t!("schedule.result.bad_arguments"),
        EXIT_FAILED => t!("schedule.result.failed"),
        EXIT_CANCELLED => t!("schedule.result.cancelled"),
        TASK_RUNNING => t!("schedule.status.running"),
        _ => t!("schedule.result.code", code = format!("0x{code:08X}")),
    }
}

fn format_time(time: NaiveDateTime) -> String {
    time.format(&t!("schedule.datetime_format")).to_string()
}

/// Пункты набора в порядке каталога.
fn ordered(tasks: &[CleanupTask], selection: &Selection) -> Vec<String> {
    tasks.iter().filter(|task| selection.contains(task.id)).map(|task| task.id.to_owned()).collect()
}

/// Названия пунктов — для подсказки.
fn task_titles(tasks: &[CleanupTask], ids: &[String]) -> String {
    let titles: Vec<String> =
        tasks.iter().filter(|task| ids.iter().any(|id| id == task.id)).map(|task| format!("• {}", task.title())).collect();
    titles.join("\n")
}

/// Названия заданий в Планировщике не различают регистр.
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// «Очистка по расписанию», «Очистка по расписанию 2» — первое свободное название.
fn unique_name(settings: &Settings) -> String {
    let base = t!("schedule.default_name");
    let taken = |name: &str| settings.schedules.iter().any(|schedule| same_name(&schedule.name, name));
    (1..)
        .map(|number| if number == 1 { base.clone() } else { format!("{base} {number}") })
        .find(|name| !taken(name))
        .unwrap_or(base)
}

/// Открывает «Планировщик заданий». Путь к оснастке полный: просто `taskschd.msc`
/// проводник ищет в своей рабочей папке, не находит и открывает «Документы».
fn open_task_scheduler() {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let console = std::path::Path::new(&windir).join(r"System32\taskschd.msc");
    let _ = Command::new("explorer").arg(console).spawn();
}

/// Регистрирует задание в Планировщике: запускать будет этот же `HexCleaner.exe`.
fn register(scheduler: &Scheduler, schedule: &Schedule) -> windows::core::Result<()> {
    let exe = std::env::current_exe().map_err(|error| windows::core::Error::new(windows::core::HRESULT(-1), error.to_string()))?;
    scheduler.register(&schedule.name, &schedule.task_xml(&exe, Local::now().date_naive()))
}
