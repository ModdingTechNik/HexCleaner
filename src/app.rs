//! Главное окно программы.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::collapsing_header::CollapsingState;
use eframe::egui::{
    self, Align, Button, CentralPanel, Checkbox, Color32, ComboBox, Grid, Key, Layout, Panel, RichText, ScrollArea,
    TextEdit, Ui, vec2,
};

use crate::catalog::{self, CleanupTask, Safety};
use crate::cleaner::{
    self, CleanupEvent, CleanupJob, CleanupOptions, Location, Mode, TaskReport, TaskResult, format_size,
};
use crate::i18n::{self, LanguageInfo};
use crate::presets::{self, ActivePreset, Selection};
use crate::settings::{Settings, Theme};
use crate::system::admin;
use crate::t;
use crate::widgets::{dot, dot_atom, shield_atom, shield_icon};

/// Цвета результатов, подобранные под тёмную и светлую темы.
struct Palette {
    /// Итоги очистки.
    success: Color32,
    /// Итоги анализа.
    found: Color32,
    error: Color32,
}

impl Palette {
    fn of(ui: &Ui) -> Self {
        if ui.visuals().dark_mode {
            Self {
                success: Color32::from_rgb(0x7C, 0xC4, 0x7F),
                found: Color32::from_rgb(0xE8, 0xB9, 0x4A),
                error: Color32::from_rgb(0xE5, 0x6B, 0x6B),
            }
        } else {
            Self {
                success: Color32::from_rgb(0x2E, 0x7D, 0x32),
                found: Color32::from_rgb(0xA8, 0x5F, 0x00),
                error: Color32::from_rgb(0xC6, 0x28, 0x28),
            }
        }
    }

    /// Цвет точки уровня безопасности: зелёный, жёлтый, красный.
    fn safety(&self, safety: Safety) -> Color32 {
        match safety {
            Safety::Safe => self.success,
            Safety::Moderate => self.found,
            Safety::Caution => self.error,
        }
    }
}

/// Что пользователь выбрал в списке наборов.
enum PresetAction {
    Apply(Selection),
    Delete(String),
    /// Показать поле для названия нового набора.
    StartSaving,
}

/// Сколько папок показывать в разбивке пункта; полный список — в журнале.
const MAX_SHOWN_LOCATIONS: usize = 15;

/// Состояние пункта в текущем (или последнем) анализе или очистке.
#[derive(Clone)]
enum TaskState {
    /// Ещё не запускался или пункт не выбран.
    Idle,
    Running(TaskReport),
    Done(TaskResult),
    /// Пропущен из-за отсутствия прав администратора.
    Skipped,
}

/// Строка состояния внизу окна.
enum Status {
    Ready,
    Running(Mode),
    Finished { mode: Mode, total: TaskReport, cancelled: bool },
    Error(String),
}

pub struct HexCleanerApp {
    tasks: Arc<Vec<CleanupTask>>,
    settings: Settings,
    is_admin: bool,
    /// Состояние каждого пункта; индексы совпадают с `tasks`.
    task_states: Vec<TaskState>,
    job: Option<CleanupJob>,
    /// Режим текущего или последнего запуска: от него зависят подписи результатов.
    mode: Mode,
    status: Status,
    last_log: Option<PathBuf>,
    /// Языки для списка выбора: встроенные и найденные в папке языков.
    languages: Vec<LanguageInfo>,
    /// Название нового набора, пока пользователь его вводит.
    new_preset_name: Option<String>,
    /// Поставить курсор в поле названия набора (один раз, когда поле появилось).
    focus_preset_name: bool,
}

impl HexCleanerApp {
    /// `language_error` — что пошло не так при включении сохранённого языка (показывается внизу окна).
    pub fn new(
        cc: &eframe::CreationContext,
        settings: Settings,
        is_admin: bool,
        language_error: Option<String>,
    ) -> Self {
        cc.egui_ctx.set_theme(egui_theme(settings.theme));
        use_system_font(&cc.egui_ctx);

        let tasks = catalog::all_tasks();
        let mut app = Self {
            task_states: vec![TaskState::Idle; tasks.len()],
            tasks: Arc::new(tasks),
            settings,
            is_admin,
            job: None,
            mode: Mode::Clean,
            status: Status::Ready,
            last_log: None,
            languages: Vec::new(),
            new_preset_name: None,
            focus_preset_name: false,
        };
        app.refresh_languages();
        if let Some(error) = language_error {
            app.status = Status::Error(error);
        }
        app
    }

    fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Можно ли выполнить пункт при текущих правах.
    fn is_available(&self, task: &CleanupTask) -> bool {
        !task.requires_admin || self.is_admin
    }

    /// Отмечен ли пункт. Пункты, которые пользователь ещё не трогал, отмечены,
    /// если они входят в рекомендуемый набор.
    fn is_selected(&self, task: &CleanupTask) -> bool {
        self.settings.is_selected(task.id).unwrap_or(task.safety.selected_by_default())
    }

    /// Все отмеченные пункты, включая недоступные без прав администратора.
    fn current_selection(&self) -> Selection {
        self.tasks.iter().filter(|task| self.is_selected(task)).map(|task| task.id.to_owned()).collect()
    }

    /// Расставляет галочки по набору — у всех пунктов, включая недоступные без прав.
    fn apply_selection(&mut self, selection: &Selection) {
        for task in self.tasks.iter() {
            self.settings.set_selected(task.id, selection.contains(task.id));
        }
        self.settings.save();
    }

    // --- Действия ---

    /// Запускает очистку или анализ отмеченных пунктов.
    fn start(&mut self, mode: Mode) {
        // Передаём все отмеченные пункты: недоступные без прав движок пропустит и отметит в журнале.
        let selected: Vec<usize> = (0..self.tasks.len())
            .filter(|&index| self.is_selected(&self.tasks[index]))
            .collect();

        self.task_states.fill(TaskState::Idle);
        self.mode = mode;
        self.status = Status::Running(mode);
        let options = CleanupOptions {
            mode,
            is_admin: self.is_admin,
            compress_old_logs: self.settings.compress_old_logs,
        };
        self.job = Some(CleanupJob::start(Arc::clone(&self.tasks), selected, options));
    }

    fn restart_as_admin(&mut self, ctx: &egui::Context) {
        self.settings.save();
        if admin::restart_as_admin() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            self.status = Status::Error(t!("status.elevation_failed"));
        }
    }

    /// Перечитывает список языков, чтобы в нём появились новые файлы из папки языков.
    fn refresh_languages(&mut self) {
        let (languages, broken_files) = i18n::available_languages();
        self.languages = languages;
        if !broken_files.is_empty() {
            self.status = Status::Error(t!("status.broken_language_files", files = broken_files.join(", ")));
        }
    }

    fn change_language(&mut self, ctx: &egui::Context, code: String) {
        match i18n::set_language(&code) {
            Ok(()) => {
                self.settings.language = Some(code);
                self.settings.save();
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(window_title(self.is_admin)));
            }
            Err(error) => self.status = Status::Error(t!("status.language_failed", code = code, error = error)),
        }
    }

    fn change_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        self.settings.theme = theme;
        self.settings.save();
        ctx.set_theme(egui_theme(theme));
    }

    /// Код языка, который сейчас включён.
    fn current_language(&self) -> String {
        self.settings.language.clone().unwrap_or_else(|| i18n::system_language().to_owned())
    }

    /// Выбрать или снять все пункты, доступные при текущих правах.
    /// Выбор недоступных пунктов не меняется, чтобы не потерять его.
    fn set_all_available(&mut self, selected: bool) {
        let available: Vec<&str> =
            self.tasks.iter().filter(|task| self.is_available(task)).map(|task| task.id).collect();
        for id in available {
            self.settings.set_selected(id, selected);
        }
        self.settings.save();
    }

    /// Обрабатывает сообщения от потока очистки.
    fn process_cleanup_events(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };

        let mut finished = false;
        for event in job.poll_events() {
            match event {
                CleanupEvent::TaskStarted(index) => self.task_states[index] = TaskState::Running(TaskReport::default()),
                CleanupEvent::Progress(index, report) => self.task_states[index] = TaskState::Running(report),
                CleanupEvent::TaskFinished(index, result) => self.task_states[index] = TaskState::Done(result),
                CleanupEvent::TaskSkipped(index) => self.task_states[index] = TaskState::Skipped,
                CleanupEvent::Finished { total, log_path, cancelled } => {
                    self.status = Status::Finished { mode: self.mode, total, cancelled };
                    self.last_log = Some(log_path);
                    finished = true;
                }
                CleanupEvent::Failed(message) => {
                    self.status = Status::Error(message);
                    finished = true;
                }
            }
        }

        if finished {
            self.job = None;
        } else {
            // Пока идёт очистка, обновляем окно, чтобы счётчики «бежали».
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Сумма по всем пунктам — для строки состояния во время очистки.
    fn running_total(&self) -> TaskReport {
        let mut total = TaskReport::default();
        for state in &self.task_states {
            match state {
                TaskState::Running(report) => total += *report,
                TaskState::Done(result) => total += result.report,
                TaskState::Idle | TaskState::Skipped => {}
            }
        }
        total
    }

    // --- Интерфейс ---

    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading(RichText::new(t!("app.name")).strong());
            ui.label(RichText::new(t!("header.subtitle")).weak());

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.is_admin {
                    // Как в Windows Terminal: щит показывает, что программа работает с правами администратора.
                    ui.label(t!("header.admin"));
                    shield_icon(ui, 18.0);
                } else {
                    let button = Button::new((shield_atom(16.0), t!("header.run_as_admin")));
                    let response = ui
                        .add_enabled(!self.is_running(), button)
                        .on_hover_text(t!("header.run_as_admin.hint"));
                    if response.clicked() {
                        self.restart_as_admin(ui.ctx());
                    }
                }

                ui.separator();
                // Во время очистки язык не меняем: журнал пишется на одном языке.
                ui.add_enabled_ui(!self.is_running(), |ui| {
                    self.theme_selector(ui);
                    self.language_selector(ui);
                });
            });
        });
    }

    fn theme_selector(&mut self, ui: &mut Ui) {
        let mut theme = self.settings.theme;
        ComboBox::from_id_salt("theme")
            .selected_text(theme_name(theme))
            .show_ui(ui, |ui| {
                for option in [Theme::Dark, Theme::Light, Theme::System] {
                    ui.selectable_value(&mut theme, option, theme_name(option));
                }
            })
            .response
            .on_hover_text(t!("header.theme.hint"));

        if theme != self.settings.theme {
            self.change_theme(ui.ctx(), theme);
        }
    }

    fn language_selector(&mut self, ui: &mut Ui) {
        let current = self.current_language();
        let current_name = self
            .languages
            .iter()
            .find(|language| language.code == current)
            .map_or(current.clone(), |language| language.name.clone());

        let mut chosen = None;
        let combo = ComboBox::from_id_salt("language").selected_text(current_name).show_ui(ui, |ui| {
            for language in &self.languages {
                if ui.selectable_label(language.code == current, &language.name).clicked() {
                    chosen = Some(language.code.clone());
                }
            }
            ui.separator();
            let folder = ui.button(t!("header.languages_folder")).on_hover_text(t!("header.languages_folder.hint"));
            if folder.clicked() {
                i18n::export_built_in_languages();
                open_in_explorer(&i18n::languages_directory());
            }
        });

        // Список перечитывается при каждом открытии — так подхватываются новые файлы переводов.
        if combo.response.clicked() {
            self.refresh_languages();
        }
        combo.response.on_hover_text(t!("header.language.hint"));

        if let Some(code) = chosen {
            self.change_language(ui.ctx(), code);
        }
    }

    fn task_list(&mut self, ui: &mut Ui) {
        if self.new_preset_name.is_some() {
            self.preset_name_editor(ui);
        } else {
            self.selection_toolbar(ui);
        }
        if !self.is_admin {
            ui.label(RichText::new(t!("list.admin_hint")).weak());
        }
        ui.separator();

        let tasks = Arc::clone(&self.tasks);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (index, task) in tasks.iter().enumerate() {
                self.task_entry(ui, index, task);
            }
        });
    }

    /// Строка над списком: набор галочек, «Отметить все» и легенда уровней безопасности.
    fn selection_toolbar(&mut self, ui: &mut Ui) {
        let all_selected = self.tasks.iter().filter(|task| self.is_available(task)).all(|task| self.is_selected(task));

        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.is_running(), |ui| {
                ui.label(t!("presets.label"));
                self.preset_selector(ui);

                let label = if all_selected { t!("list.unselect_all") } else { t!("list.select_all") };
                if ui.button(label).clicked() {
                    self.set_all_available(!all_selected);
                }
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let palette = Palette::of(ui);
                // Справа налево: последний уровень оказывается крайним справа.
                for level in Safety::ALL.into_iter().rev() {
                    ui.label(safety_name(level)).on_hover_text(safety_hint(level));
                    dot(ui, palette.safety(level));
                    ui.add_space(6.0);
                }
            });
        });
    }

    fn preset_selector(&mut self, ui: &mut Ui) {
        let active = presets::find_active(&self.tasks, &self.settings.presets, &self.current_selection());
        let selected_text = match &active {
            ActivePreset::BuiltIn(level) => preset_name(*level),
            ActivePreset::User(name) => name.clone(),
            ActivePreset::Custom => t!("presets.custom"),
        };

        let mut action = None;
        ComboBox::from_id_salt("presets").selected_text(selected_text).width(190.0).show_ui(ui, |ui| {
            let palette = Palette::of(ui);
            for level in Safety::ALL {
                let item = Button::selectable(
                    active == ActivePreset::BuiltIn(level),
                    (dot_atom(palette.safety(level)), preset_name(level)),
                );
                if ui.add(item).on_hover_text(preset_hint(level)).clicked() {
                    action = Some(PresetAction::Apply(presets::built_in(&self.tasks, level)));
                }
            }

            if !self.settings.presets.is_empty() {
                ui.separator();
            }
            for preset in &self.settings.presets {
                ui.horizontal(|ui| {
                    let is_active = active == ActivePreset::User(preset.name.clone());
                    if ui.selectable_label(is_active, &preset.name).clicked() {
                        action = Some(PresetAction::Apply(preset.tasks.clone()));
                    }
                    if ui.small_button(t!("presets.delete")).clicked() {
                        action = Some(PresetAction::Delete(preset.name.clone()));
                    }
                });
            }

            ui.separator();
            if ui.button(t!("presets.save")).clicked() {
                action = Some(PresetAction::StartSaving);
            }
        });

        match action {
            Some(PresetAction::Apply(selection)) => self.apply_selection(&selection),
            Some(PresetAction::Delete(name)) => {
                presets::delete(&mut self.settings.presets, &name);
                self.settings.save();
            }
            Some(PresetAction::StartSaving) => {
                self.new_preset_name = Some(String::new());
                self.focus_preset_name = true;
            }
            None => {}
        }
    }

    /// Поле для названия нового набора вместо строки с наборами.
    /// Enter — сохранить текущие галочки как набор, Escape — отменить.
    fn preset_name_editor(&mut self, ui: &mut Ui) {
        let mut name = self.new_preset_name.take().unwrap_or_default();
        let mut save = false;
        let mut cancel = false;

        ui.horizontal(|ui| {
            ui.label(t!("presets.name_label"));
            let edit =
                ui.add(TextEdit::singleline(&mut name).hint_text(t!("presets.name_hint")).desired_width(220.0));
            if self.focus_preset_name {
                edit.request_focus();
                self.focus_preset_name = false;
            }

            let is_valid = !name.trim().is_empty();
            let enter_pressed = edit.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            let save_clicked = ui.add_enabled(is_valid, Button::new(t!("presets.save_button"))).clicked();
            save = is_valid && (save_clicked || enter_pressed);
            cancel = ui.button(t!("presets.cancel")).clicked() || ui.input(|input| input.key_pressed(Key::Escape));
        });

        if save {
            let selection = self.current_selection();
            presets::save(&mut self.settings.presets, &name, selection);
            self.settings.save();
        } else if !cancel {
            self.new_preset_name = Some(name);
        }
    }

    /// Пункт списка. Если для него известна разбивка по папкам, его можно развернуть.
    fn task_entry(&mut self, ui: &mut Ui, index: usize, task: &CleanupTask) {
        let has_locations =
            matches!(&self.task_states[index], TaskState::Done(result) if !result.locations.is_empty());
        if !has_locations {
            ui.horizontal(|ui| {
                // Отступ на месте треугольника, чтобы все пункты стояли ровно.
                ui.add_space(ui.spacing().indent);
                self.task_row(ui, index, task);
            });
            return;
        }

        let id = ui.make_persistent_id(("task_locations", task.id));
        CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| self.task_row(ui, index, task))
            .body(|ui| {
                if let TaskState::Done(result) = &self.task_states[index] {
                    locations_grid(ui, task.id, &result.locations);
                }
            });
    }

    fn task_row(&mut self, ui: &mut Ui, index: usize, task: &CleanupTask) {
        let palette = Palette::of(ui);
        let mut selected = self.is_selected(task);
        let safety_dot = dot_atom(palette.safety(task.safety));
        let checkbox = if task.requires_admin {
            Checkbox::new(&mut selected, (safety_dot, task.title(), shield_atom(14.0)))
        } else {
            Checkbox::new(&mut selected, (safety_dot, task.title()))
        };

        let enabled = self.is_available(task) && !self.is_running();
        let safety_line = t!("list.safety_line", level = safety_name(task.safety), hint = safety_hint(task.safety));
        let description = format!("{}\n\n{safety_line}", task.description());
        let response = ui
            .add_enabled(enabled, checkbox)
            .on_hover_text(&description)
            .on_disabled_hover_text(if self.is_available(task) {
                description.clone()
            } else {
                t!("list.needs_admin", description = description)
            });
        if response.changed() {
            self.settings.set_selected(task.id, selected);
            self.settings.save();
        }

        let result_color = match self.mode {
            Mode::Clean => palette.success,
            Mode::Analyze => palette.found,
        };
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Отступ под полосу прокрутки, чтобы она не закрывала цифры.
            ui.add_space(ui.spacing().scroll.bar_width + 4.0);
            Self::task_state_label(ui, &self.task_states[index], result_color);
        });
    }

    /// Итог пункта справа в строке: счётчики, пояснение или «пропущено».
    fn task_state_label(ui: &mut Ui, state: &TaskState, result_color: Color32) {
        match state {
            TaskState::Idle => {}
            TaskState::Running(report) => {
                ui.label(report.summary());
                ui.spinner();
            }
            TaskState::Done(TaskResult { note: Some(note), .. }) => {
                ui.label(RichText::new(note).weak());
            }
            TaskState::Done(result) => {
                ui.label(RichText::new(result.report.summary()).color(result_color));
            }
            TaskState::Skipped => {
                ui.label(RichText::new(t!("list.skipped")).weak());
            }
        }
    }

    fn footer(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                if ui.add(Button::new(t!("footer.stop")).min_size(vec2(150.0, 30.0))).clicked() {
                    job.cancel();
                }
            } else {
                let has_selection = self
                    .tasks
                    .iter()
                    .any(|task| self.is_available(task) && self.is_selected(task));

                let analyze = Button::new(t!("footer.analyze")).min_size(vec2(110.0, 30.0));
                let response = ui.add_enabled(has_selection, analyze).on_hover_text(t!("footer.analyze.hint"));
                if response.clicked() {
                    self.start(Mode::Analyze);
                }

                let clean = Button::new(RichText::new(t!("footer.clean")).strong()).min_size(vec2(150.0, 30.0));
                if ui.add_enabled(has_selection, clean).clicked() {
                    self.start(Mode::Clean);
                }
            }

            if let Some(log_path) = &self.last_log
                && ui.button(t!("footer.open_log")).clicked()
            {
                open_in_explorer(log_path);
            }
            if ui.button(t!("footer.logs_folder")).clicked() {
                let dir = cleaner::logs_directory();
                let _ = std::fs::create_dir_all(&dir);
                open_in_explorer(&dir);
            }

            let enabled = !self.is_running();
            let compress = Checkbox::new(&mut self.settings.compress_old_logs, t!("footer.compress_logs"));
            let response = ui.add_enabled(enabled, compress).on_hover_text(t!("footer.compress_logs.hint"));
            if response.changed() {
                self.settings.save();
            }
        });

        ui.add_space(4.0);
        self.status_line(ui);
        ui.add_space(6.0);
    }

    fn status_line(&self, ui: &mut Ui) {
        match &self.status {
            Status::Ready => {
                ui.label(RichText::new(t!("status.ready")).weak());
            }
            Status::Running(mode) => {
                let summary = self.running_total().summary();
                let text = match mode {
                    Mode::Clean => t!("status.running_clean", summary = summary),
                    Mode::Analyze => t!("status.running_analysis", summary = summary),
                };
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(text);
                });
            }
            Status::Finished { mode: Mode::Analyze, total, cancelled } => {
                let (files, size) = (total.deleted_files, format_size(total.freed_bytes));
                let text = if *cancelled {
                    t!("status.analysis_stopped", files = files, size = size)
                } else {
                    t!("status.analysis_done", files = files, size = size)
                };
                ui.label(RichText::new(text).strong());
            }
            Status::Finished { mode: Mode::Clean, total, cancelled } => {
                let (files, size) = (total.deleted_files, format_size(total.freed_bytes));
                let mut text = if *cancelled {
                    t!("status.clean_stopped", files = files, size = size)
                } else {
                    t!("status.clean_done", files = files, size = size)
                };
                if total.freed_memory > 0 {
                    text += &t!("status.freed_memory", size = format_size(total.freed_memory));
                }
                if total.failed > 0 {
                    text += &t!("status.failed", count = total.failed);
                }
                ui.label(RichText::new(text).strong());
            }
            Status::Error(message) => {
                ui.label(RichText::new(message).color(Palette::of(ui).error));
            }
        }
    }
}

impl eframe::App for HexCleanerApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.process_cleanup_events(ui.ctx());

        Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            self.header(ui);
            ui.add_space(6.0);
        });
        Panel::bottom("footer").show(ui, |ui| self.footer(ui));
        CentralPanel::default().show(ui, |ui| self.task_list(ui));
    }
}

/// Таблица «объём — число файлов — папка» для раскрытого пункта.
fn locations_grid(ui: &mut Ui, task_id: &str, locations: &[Location]) {
    Grid::new(("locations", task_id)).num_columns(3).spacing([16.0, 2.0]).show(ui, |ui| {
        for location in locations.iter().take(MAX_SHOWN_LOCATIONS) {
            ui.label(RichText::new(format_size(location.bytes)).strong());
            ui.label(t!("locations.files", count = location.files));
            let hint = t!("locations.open_hint", path = location.path.display());
            if ui.link(short_path(&location.path)).on_hover_text(hint).clicked() {
                open_in_explorer(&location.path);
            }
            ui.end_row();
        }
    });

    let hidden = locations.len().saturating_sub(MAX_SHOWN_LOCATIONS);
    if hidden > 0 {
        ui.label(RichText::new(t!("locations.more", count = hidden)).weak());
    }
}

/// Сокращает путь, заменяя известные папки переменными: `%LOCALAPPDATA%\Google\Chrome\...`.
fn short_path(path: &Path) -> String {
    let full = path.display().to_string();
    // Сначала более глубокие папки: %TEMP% лежит внутри %LOCALAPPDATA%, а та — внутри %USERPROFILE%.
    for variable in ["TEMP", "LOCALAPPDATA", "APPDATA", "USERPROFILE", "PROGRAMDATA", "WINDIR"] {
        let Ok(prefix) = std::env::var(variable) else { continue };
        let prefix = prefix.trim_end_matches('\\');
        let matches = full.get(..prefix.len()).is_some_and(|start| start.eq_ignore_ascii_case(prefix));
        if !prefix.is_empty() && matches {
            return format!("%{variable}%{}", &full[prefix.len()..]);
        }
    }
    full
}

/// Заголовок окна на текущем языке.
pub fn window_title(is_admin: bool) -> String {
    if is_admin { t!("window.title_admin") } else { t!("window.title") }
}

fn safety_name(safety: Safety) -> String {
    match safety {
        Safety::Safe => t!("safety.safe"),
        Safety::Moderate => t!("safety.moderate"),
        Safety::Caution => t!("safety.caution"),
    }
}

fn safety_hint(safety: Safety) -> String {
    match safety {
        Safety::Safe => t!("safety.safe.hint"),
        Safety::Moderate => t!("safety.moderate.hint"),
        Safety::Caution => t!("safety.caution.hint"),
    }
}

/// Название встроенного набора, который включает пункты не опаснее `level`.
fn preset_name(level: Safety) -> String {
    match level {
        Safety::Safe => t!("presets.safe"),
        Safety::Moderate => t!("presets.recommended"),
        Safety::Caution => t!("presets.full"),
    }
}

fn preset_hint(level: Safety) -> String {
    match level {
        Safety::Safe => t!("presets.safe.hint"),
        Safety::Moderate => t!("presets.recommended.hint"),
        Safety::Caution => t!("presets.full.hint"),
    }
}

fn theme_name(theme: Theme) -> String {
    match theme {
        Theme::Dark => t!("theme.dark"),
        Theme::Light => t!("theme.light"),
        Theme::System => t!("theme.system"),
    }
}

fn egui_theme(theme: Theme) -> egui::ThemePreference {
    match theme {
        Theme::Dark => egui::ThemePreference::Dark,
        Theme::Light => egui::ThemePreference::Light,
        Theme::System => egui::ThemePreference::System,
    }
}

/// Открывает файл в программе по умолчанию или папку в проводнике.
fn open_in_explorer(path: &Path) {
    let _ = Command::new("explorer").arg(path).spawn();
}

/// Шрифт Segoe UI, как у остальных программ Windows (если он есть в системе).
fn use_system_font(ctx: &egui::Context) {
    let Ok(font) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("Segoe UI".to_owned(), Arc::new(egui::FontData::from_owned(font)));
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "Segoe UI".to_owned());
    ctx.set_fonts(fonts);
}
