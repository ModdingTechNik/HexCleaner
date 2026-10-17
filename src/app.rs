//! Главное окно программы.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{
    self, Align, Button, CentralPanel, Checkbox, Color32, Layout, Panel, RichText, ScrollArea, Ui, vec2,
};

use crate::catalog::{self, CleanupTask};
use crate::cleaner::{self, CleanupEvent, CleanupJob, TaskReport, format_size};
use crate::settings::Settings;
use crate::system::admin;
use crate::widgets::{shield_atom, shield_icon};

const ERROR_COLOR: Color32 = Color32::from_rgb(0xE5, 0x6B, 0x6B);
const SUCCESS_COLOR: Color32 = Color32::from_rgb(0x7C, 0xC4, 0x7F);

/// Состояние пункта в текущей (или последней) очистке.
#[derive(Clone, Copy)]
enum TaskState {
    /// Очистка ещё не запускалась или пункт не выбран.
    Idle,
    Running(TaskReport),
    Done(TaskReport),
    /// Пропущен из-за отсутствия прав администратора.
    Skipped,
}

/// Строка состояния внизу окна.
enum Status {
    Ready,
    Running,
    Finished { total: TaskReport, cancelled: bool },
    Error(String),
}

pub struct HexCleanerApp {
    tasks: Arc<Vec<CleanupTask>>,
    settings: Settings,
    is_admin: bool,
    /// Состояние каждого пункта; индексы совпадают с `tasks`.
    task_states: Vec<TaskState>,
    job: Option<CleanupJob>,
    status: Status,
    last_log: Option<PathBuf>,
}

impl HexCleanerApp {
    pub fn new(cc: &eframe::CreationContext, is_admin: bool) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        use_system_font(&cc.egui_ctx);

        let tasks = catalog::all_tasks();
        Self {
            task_states: vec![TaskState::Idle; tasks.len()],
            tasks: Arc::new(tasks),
            settings: Settings::load(),
            is_admin,
            job: None,
            status: Status::Ready,
            last_log: None,
        }
    }

    fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Можно ли выполнить пункт при текущих правах.
    fn is_available(&self, task: &CleanupTask) -> bool {
        !task.requires_admin || self.is_admin
    }

    // --- Действия ---

    fn start_cleanup(&mut self) {
        // Передаём все отмеченные пункты: недоступные без прав движок пропустит и отметит в журнале.
        let selected: Vec<usize> = (0..self.tasks.len())
            .filter(|&index| self.settings.is_selected(self.tasks[index].id))
            .collect();

        self.task_states.fill(TaskState::Idle);
        self.status = Status::Running;
        self.job = Some(CleanupJob::start(Arc::clone(&self.tasks), selected, self.is_admin));
    }

    fn restart_as_admin(&mut self, ctx: &egui::Context) {
        self.settings.save();
        match admin::restart_as_admin() {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(message) => self.status = Status::Error(message),
        }
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
                CleanupEvent::TaskFinished(index, report) => self.task_states[index] = TaskState::Done(report),
                CleanupEvent::TaskSkipped(index) => self.task_states[index] = TaskState::Skipped,
                CleanupEvent::Finished { total, log_path, cancelled } => {
                    self.status = Status::Finished { total, cancelled };
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
            if let TaskState::Running(report) | TaskState::Done(report) = state {
                total += *report;
            }
        }
        total
    }

    // --- Интерфейс ---

    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading(RichText::new("HexCleaner").strong());
            ui.label(RichText::new("очистка мусора и кэшей Windows").weak());

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.is_admin {
                    // Как в Windows Terminal: щит показывает, что программа работает с правами администратора.
                    ui.label("Администратор");
                    shield_icon(ui, 18.0);
                } else {
                    let button = Button::new((shield_atom(16.0), "Запустить от администратора"));
                    let response = ui
                        .add_enabled(!self.is_running(), button)
                        .on_hover_text("Перезапустить программу с правами администратора (UAC).");
                    if response.clicked() {
                        self.restart_as_admin(ui.ctx());
                    }
                }
            });
        });
    }

    fn task_list(&mut self, ui: &mut Ui) {
        let all_selected = self
            .tasks
            .iter()
            .filter(|task| self.is_available(task))
            .all(|task| self.settings.is_selected(task.id));

        ui.horizontal(|ui| {
            let label = if all_selected { "Снять все галочки" } else { "Отметить все" };
            if ui.add_enabled(!self.is_running(), Button::new(label)).clicked() {
                self.set_all_available(!all_selected);
            }
            if !self.is_admin {
                ui.label(RichText::new("Пункты со щитом требуют прав администратора и будут пропущены.").weak());
            }
        });
        ui.separator();

        let tasks = Arc::clone(&self.tasks);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (index, task) in tasks.iter().enumerate() {
                ui.horizontal(|ui| self.task_row(ui, index, task));
            }
        });
    }

    fn task_row(&mut self, ui: &mut Ui, index: usize, task: &CleanupTask) {
        let mut selected = self.settings.is_selected(task.id);
        let checkbox = if task.requires_admin {
            Checkbox::new(&mut selected, (task.title, shield_atom(14.0)))
        } else {
            Checkbox::new(&mut selected, task.title)
        };

        let enabled = self.is_available(task) && !self.is_running();
        let response = ui
            .add_enabled(enabled, checkbox)
            .on_hover_text(task.description)
            .on_disabled_hover_text(if self.is_available(task) {
                task.description.to_owned()
            } else {
                format!("Нужны права администратора.\n\n{}", task.description)
            });
        if response.changed() {
            self.settings.set_selected(task.id, selected);
            self.settings.save();
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match self.task_states[index] {
            TaskState::Idle => {}
            TaskState::Running(report) => {
                ui.label(report.summary());
                ui.spinner();
            }
            TaskState::Done(report) => {
                ui.label(RichText::new(report.summary()).color(SUCCESS_COLOR));
            }
            TaskState::Skipped => {
                ui.label(RichText::new("пропущено: нужны права администратора").weak());
            }
        });
    }

    fn footer(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                if ui.add(Button::new("Остановить").min_size(vec2(150.0, 30.0))).clicked() {
                    job.cancel();
                }
            } else {
                let has_selection = self
                    .tasks
                    .iter()
                    .any(|task| self.is_available(task) && self.settings.is_selected(task.id));
                let button = Button::new(RichText::new("Начать очистку").strong()).min_size(vec2(150.0, 30.0));
                if ui.add_enabled(has_selection, button).clicked() {
                    self.start_cleanup();
                }
            }

            if let Some(log_path) = &self.last_log
                && ui.button("Открыть журнал").clicked()
            {
                open_in_explorer(log_path);
            }
            if ui.button("Папка журналов").clicked() {
                let dir = cleaner::logs_directory();
                let _ = std::fs::create_dir_all(&dir);
                open_in_explorer(&dir);
            }
        });

        ui.add_space(4.0);
        self.status_line(ui);
        ui.add_space(6.0);
    }

    fn status_line(&self, ui: &mut Ui) {
        match &self.status {
            Status::Ready => {
                ui.label(RichText::new("Отметьте нужные пункты и нажмите «Начать очистку».").weak());
            }
            Status::Running => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Идёт очистка… {}", self.running_total().summary()));
                });
            }
            Status::Finished { total, cancelled } => {
                let mut text = format!(
                    "{}: удалено файлов {}, освобождено {}",
                    if *cancelled { "Остановлено" } else { "Готово" },
                    total.deleted_files,
                    format_size(total.freed_bytes)
                );
                if total.freed_memory > 0 {
                    text += &format!(", ОЗУ {}", format_size(total.freed_memory));
                }
                if total.failed > 0 {
                    text += &format!(". Не удалось удалить {} (заняты или нет доступа)", total.failed);
                }
                ui.label(RichText::new(text).strong());
            }
            Status::Error(message) => {
                ui.label(RichText::new(message).color(ERROR_COLOR));
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
