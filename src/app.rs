//! Главное окно программы.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use eframe::egui::collapsing_header::CollapsingState;
use eframe::egui::{
    self, Align, Button, CentralPanel, Checkbox, Color32, ComboBox, Grid, Id, Key, Label, Layout, Modal, Panel,
    PointerButton, Popup, ProgressBar, RichText, ScrollArea, TextEdit, TextStyle, TextWrapMode, Ui, WidgetText, vec2,
};

use hexcleaner::catalog::{self, CleanupTask, Safety};
use hexcleaner::cleaner::{
    self, BlockingApp, CleanupEvent, CleanupJob, CleanupOptions, Location, Mode, TaskReport, TaskResult,
    find_blocking_apps, format_size,
};
use hexcleaner::presets::{self, ActivePreset, Selection};
use hexcleaner::settings::Settings;
use hexcleaner::system::admin;
use hexcleaner::system::notification::{self, Notification};
use hexcleaner::trash::{LimitRequest, RestoreReport, Trash, TrashDecision, TrashPolicy};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use hexcleaner::t;

use crate::disk_tab::DiskTab;
use crate::progress::{CleanupProgress, TaskProgress, cleanup_progress};
use crate::schedule_tab::ScheduleTab;
use crate::settings_tab::{SettingsTab, egui_theme};
use crate::trash_tab::{JobOwner, JobResult, TrashTab, TrashWorker, restore_summary};
use crate::update_ui::Updater;
use crate::widgets::{dot, dot_atom, shield_atom, shield_icon, undo_atom, warning_atom};

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

/// Высота кнопок внизу вкладки «Очистка».
const FOOTER_BUTTON_HEIGHT: f32 = 30.0;

/// Ширина кнопок «Начать очистку» и «Остановить».
const CLEAN_BUTTON_WIDTH: f32 = 150.0;

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

/// Вкладки окна.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Cleanup,
    Disk,
    Trash,
    Schedule,
    Settings,
}

/// Строка состояния внизу окна.
enum Status {
    Ready,
    /// Перед очисткой ищем программы, которые ей мешают.
    CheckingPrograms,
    /// Закрываем программы, которые мешают очистке.
    ClosingPrograms,
    Running(Mode),
    /// Файлы очистки упаковываются в корзине.
    PackingTrash,
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
    /// Название нового набора, пока пользователь его вводит.
    new_preset_name: Option<String>,
    /// Поставить курсор в поле названия набора (один раз, когда поле появилось).
    focus_preset_name: bool,
    /// Перед очисткой: поиск программ, которые ей мешают, и диалог с предложением их закрыть.
    programs_check: ProgramsCheck,
    /// Дескриптор окна Windows — нужен для уведомлений.
    window: Option<isize>,
    /// Последнее уведомление о завершении. Его значок в области уведомлений живёт,
    /// пока не придёт следующее уведомление или не закроется программа: если убрать
    /// значок раньше, Windows уберёт и само сообщение.
    notification: Option<Notification>,
    /// Итоги последнего анализа по пунктам (индексы как у `tasks`): из них считается
    /// прогресс следующей очистки. После очистки устаревают и сбрасываются.
    analysis: Vec<Option<TaskReport>>,
    /// Итоги анализа для пунктов текущей очистки; у остальных пунктов — `None`.
    expected: Vec<Option<TaskReport>>,
    tab: Tab,
    /// Нажатые боковые кнопки мыши (`true` — «Вперёд»), ещё не обработанные.
    side_buttons: Vec<bool>,
    disk_tab: DiskTab,
    trash_tab: TrashTab,
    /// Восстановление и удаление в корзине — по одной операции за раз.
    trash_worker: TrashWorker,
    /// Очистка в корзине, файлы которой можно вернуть кнопками у пунктов. Сбрасывается
    /// новым анализом или очисткой — дальше вернуть их можно на вкладке «Корзина».
    undo_run: Option<String>,
    /// Пункты, файлы которых уже вернули, и итог.
    restored: HashMap<usize, RestoreReport>,
    /// Файлы не помещаются в лимит корзины: очистка ждёт ответа.
    trash_limit: Option<LimitRequest>,
    /// Очистку начали с зажатым Shift: удалить мимо корзины.
    bypass_trash: bool,
    schedule_tab: ScheduleTab,
    settings_tab: SettingsTab,
    updater: Updater,
}

/// Этап проверки программ, мешающих очистке.
enum ProgramsCheck {
    Idle,
    /// Поиск идёт в отдельном потоке — он занимает секунду-две.
    Searching(Receiver<Vec<BlockingApp>>),
    /// Диалог: какие программы закрыть (галочки) и запускать ли их снова после очистки.
    Asking { apps: Vec<(BlockingApp, bool)>, restart: bool },
}

/// Что пользователь выбрал в диалоге закрытия программ.
enum ProgramsDecision {
    CloseAndClean,
    CleanWithoutClosing,
    Cancel,
}

impl HexCleanerApp {
    /// `language_error` — что пошло не так при включении сохранённого языка (показывается внизу окна),
    /// `open_folder` — папка, которую сразу открыть во вкладке «Место на диске».
    pub fn new(
        cc: &eframe::CreationContext,
        settings: Settings,
        is_admin: bool,
        language_error: Option<String>,
        open_folder: Option<PathBuf>,
    ) -> Self {
        cc.egui_ctx.set_theme(egui_theme(settings.theme));
        use_system_font(&cc.egui_ctx);

        let tasks = catalog::all_tasks();
        let task_count = tasks.len();
        let mut app = Self {
            task_states: vec![TaskState::Idle; task_count],
            tasks: Arc::new(tasks),
            settings,
            is_admin,
            job: None,
            mode: Mode::Clean,
            status: Status::Ready,
            last_log: None,
            new_preset_name: None,
            focus_preset_name: false,
            programs_check: ProgramsCheck::Idle,
            window: None,
            notification: None,
            analysis: vec![None; task_count],
            expected: vec![None; task_count],
            tab: Tab::Cleanup,
            side_buttons: Vec::new(),
            disk_tab: DiskTab::default(),
            trash_tab: TrashTab::default(),
            trash_worker: TrashWorker::default(),
            undo_run: None,
            restored: HashMap::new(),
            trash_limit: None,
            bypass_trash: false,
            schedule_tab: ScheduleTab::default(),
            settings_tab: SettingsTab::new(),
            updater: Updater::new(),
        };
        app.updater.check_on_start(&app.settings);
        // Очистки старше срока хранения удаляются и без новой очистки — в фоне.
        if let Some(days) = app.settings.trash.keep_days {
            let trash = Trash::new(app.settings.trash.directory());
            thread::spawn(move || trash.remove_expired(days, chrono::Local::now()));
        }
        if let Some(error) = language_error {
            app.status = Status::Error(error);
        }
        // Запуск из контекстного меню проводника: сразу открываем папку.
        if let Some(path) = open_folder {
            app.disk_tab.open(path);
            app.tab = Tab::Disk;
        }
        app
    }

    /// Идёт очистка, анализ или подготовка к ним (поиск программ, диалог),
    /// или устанавливается обновление — после него программа перезапустится.
    fn is_running(&self) -> bool {
        self.is_cleaning() || self.updater.is_installing()
    }

    /// Идёт очистка, анализ или подготовка к ним — обновляться нельзя.
    fn is_cleaning(&self) -> bool {
        self.job.is_some() || !matches!(self.programs_check, ProgramsCheck::Idle)
    }

    /// Можно ли выполнить пункт при текущих правах.
    fn is_available(&self, task: &CleanupTask) -> bool {
        !task.requires_admin || self.is_admin
    }

    /// Отмечен ли пункт. Пункты, которые пользователь ещё не трогал, отмечены,
    /// если они входят в рекомендуемый набор.
    fn is_selected(&self, task: &CleanupTask) -> bool {
        self.settings.is_task_selected(task)
    }

    /// Все отмеченные пункты, включая недоступные без прав администратора.
    fn current_selection(&self) -> Selection {
        self.settings.selection(&self.tasks)
    }

    /// Расставляет галочки по набору — у всех пунктов, включая недоступные без прав.
    fn apply_selection(&mut self, selection: &Selection) {
        for task in self.tasks.iter() {
            self.settings.set_selected(task.id, selection.contains(task.id));
        }
        self.settings.save();
    }

    // --- Действия ---

    /// Отмеченные пункты, включая недоступные без прав: движок их пропустит и отметит в журнале.
    fn selected_indices(&self) -> Vec<usize> {
        (0..self.tasks.len()).filter(|&index| self.is_selected(&self.tasks[index])).collect()
    }

    /// Запускает очистку или анализ отмеченных пунктов. Перед очисткой закрывает
    /// программы `close_programs` и, если `restart_programs`, после очистки запускает их снова.
    fn start(&mut self, mode: Mode, close_programs: Vec<String>, restart_programs: bool) {
        // С Shift корзина для этой очистки отключается, но её папка по-прежнему не очищается.
        let mut trash = self.settings.trash.clone();
        if mode == Mode::Clean && std::mem::take(&mut self.bypass_trash) {
            trash.limit = Some(0);
        }
        // Очистке, начатой после анализа, известно, сколько файлов предстоит удалить.
        let selected = self.selected_indices();
        self.expected = (0..self.tasks.len())
            .map(|index| match mode {
                Mode::Clean if selected.contains(&index) && self.is_available(&self.tasks[index]) => self.analysis[index],
                _ => None,
            })
            .collect();

        self.task_states.fill(TaskState::Idle);
        self.undo_run = None;
        self.restored.clear();
        self.mode = mode;
        self.status = Status::Running(mode);
        let options = CleanupOptions {
            mode,
            is_admin: self.is_admin,
            log_compression: self.settings.compress_old_logs.then_some(self.settings.log_compression),
            close_programs,
            restart_programs,
            trash: Some(trash),
            ask_trash_limit: true,
        };
        self.job = Some(CleanupJob::start(Arc::clone(&self.tasks), selected, options));
    }

    /// Кнопка «Начать очистку»: сначала ищем программы, которые мешают очистке.
    fn check_programs_and_clean(&mut self) {
        let (sender, receiver) = mpsc::channel();
        let tasks = Arc::clone(&self.tasks);
        let selected = self.selected_indices();
        thread::spawn(move || {
            let _ = sender.send(find_blocking_apps(&tasks, &selected));
        });
        self.status = Status::CheckingPrograms;
        self.programs_check = ProgramsCheck::Searching(receiver);
    }

    /// Поиск программ закончился: если мешать некому — сразу чистим, иначе спрашиваем.
    fn poll_programs_check(&mut self, ctx: &egui::Context) {
        let ProgramsCheck::Searching(receiver) = &self.programs_check else { return };
        match receiver.try_recv() {
            Ok(apps) if apps.is_empty() => {
                self.programs_check = ProgramsCheck::Idle;
                self.start(Mode::Clean, Vec::new(), false);
            }
            Ok(apps) => {
                let apps = apps.into_iter().map(|app| (app, true)).collect();
                self.programs_check = ProgramsCheck::Asking { apps, restart: true };
                self.status = Status::Ready;
            }
            Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(100)),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.programs_check = ProgramsCheck::Idle;
                self.start(Mode::Clean, Vec::new(), false);
            }
        }
    }

    /// Диалог «Эти программы мешают очистке»: что закрыть и запускать ли их потом снова.
    fn programs_dialog(&mut self, ctx: &egui::Context) {
        let tasks = Arc::clone(&self.tasks);
        let ProgramsCheck::Asking { apps, restart } = &mut self.programs_check else { return };
        let mut decision = None;

        let modal = Modal::new(Id::unique("close_programs")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.heading(t!("apps.title"));
            ui.label(t!("apps.intro"));
            ui.add_space(8.0);

            for (app, close) in apps.iter_mut() {
                ui.checkbox(close, app.label());
                let blocks: Vec<String> = app.tasks.iter().map(|&index| tasks[index].title()).collect();
                ui.indent(&app.exe, |ui| {
                    ui.label(RichText::new(t!("apps.blocks", tasks = blocks.join("; "))).weak());
                });
            }

            ui.add_space(8.0);
            ui.checkbox(restart, t!("apps.restart"));
            ui.label(RichText::new(t!("apps.warning")).weak());
            ui.separator();

            ui.horizontal(|ui| {
                if ui.button(RichText::new(t!("apps.close_and_clean")).strong()).clicked() {
                    decision = Some(ProgramsDecision::CloseAndClean);
                }
                if ui.button(t!("apps.clean_without_closing")).clicked() {
                    decision = Some(ProgramsDecision::CleanWithoutClosing);
                }
                if ui.button(t!("apps.cancel")).clicked() {
                    decision = Some(ProgramsDecision::Cancel);
                }
            });
        });
        if modal.should_close() && decision.is_none() {
            decision = Some(ProgramsDecision::Cancel);
        }

        let Some(decision) = decision else { return };
        let programs: Vec<String> =
            apps.iter().filter(|(_, close)| *close).flat_map(|(app, _)| app.exes.clone()).collect();
        let restart = *restart;
        self.programs_check = ProgramsCheck::Idle;

        match decision {
            ProgramsDecision::CloseAndClean => self.start(Mode::Clean, programs, restart),
            ProgramsDecision::CleanWithoutClosing => self.start(Mode::Clean, Vec::new(), false),
            ProgramsDecision::Cancel => {
                self.bypass_trash = false;
                self.status = Status::Ready;
            }
        }
    }

    fn restart_as_admin(&mut self, ctx: &egui::Context) {
        self.settings.save();
        if admin::restart_as_admin() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            self.status = Status::Error(t!("status.elevation_failed"));
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
                CleanupEvent::ClosingPrograms => self.status = Status::ClosingPrograms,
                CleanupEvent::TaskStarted(index) => {
                    self.status = Status::Running(self.mode);
                    self.task_states[index] = TaskState::Running(TaskReport::default());
                }
                CleanupEvent::Progress(index, report) => self.task_states[index] = TaskState::Running(report),
                CleanupEvent::TaskFinished(index, result) => self.task_states[index] = TaskState::Done(result),
                CleanupEvent::TaskSkipped(index) => self.task_states[index] = TaskState::Skipped,
                CleanupEvent::TrashLimit(request) => {
                    self.trash_limit = Some(request);
                    if let Some(window) = self.window
                        && !is_focused(ctx)
                    {
                        notification::flash_taskbar(window);
                    }
                }
                CleanupEvent::PackingTrash => self.status = Status::PackingTrash,
                CleanupEvent::Finished { total, log_path, cancelled, trash_run } => {
                    self.undo_run = trash_run;
                    self.trash_tab.mark_dirty();
                    self.status = Status::Finished { mode: self.mode, total, cancelled };
                    self.last_log = Some(log_path);
                    finished = true;
                    self.remember_analysis(cancelled);
                    let (files, size) = (total.deleted_files, format_size(total.freed_bytes));
                    let text = match (self.mode, cancelled) {
                        (Mode::Clean, false) => t!("notify.clean_done", files = files, size = size),
                        (Mode::Clean, true) => t!("status.clean_stopped", files = files, size = size),
                        (Mode::Analyze, false) => t!("notify.analysis_done", files = files, size = size),
                        (Mode::Analyze, true) => t!("status.analysis_stopped", files = files, size = size),
                    };
                    self.notify(ctx, &text);
                }
                CleanupEvent::Failed(message) => {
                    self.notify(ctx, &message);
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

    /// Сообщает о завершении: в углу экрана появляется уведомление с итогом, а если окно
    /// неактивно, его кнопка на панели задач мигает, пока окно не откроют.
    /// И то и другое можно отключить во вкладке «Настройки».
    fn notify(&mut self, ctx: &egui::Context, text: &str) {
        let Some(window) = self.window else { return };
        // Команды окна eframe выполняет только при отрисовке, а свёрнутое окно не рисуется, —
        // поэтому мигание запрашиваем у Windows напрямую.
        if self.settings.flash_taskbar && !is_focused(ctx) {
            notification::flash_taskbar(window);
        }
        // Предыдущее уведомление заменяем новым: значок в области уведомлений у окна один.
        self.notification = None;
        if self.settings.notify_when_done {
            self.notification = Notification::show(window, &t!("app.name"), text);
        }
    }

    /// Новая версия установлена: запускаем её вместо себя.
    fn restart_after_update(&mut self, ctx: &egui::Context, exe: &Path) {
        self.settings.save();
        match Command::new(exe).spawn() {
            Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(_) => self.status = Status::Error(t!("update.restart_failed")),
        }
    }

    /// После анализа запоминает его итоги для прогресса следующей очистки,
    /// после очистки — сбрасывает: найденного анализом уже нет.
    fn remember_analysis(&mut self, cancelled: bool) {
        match self.mode {
            Mode::Analyze if !cancelled => {
                self.analysis = self
                    .task_states
                    .iter()
                    .map(|state| match state {
                        TaskState::Done(result) => Some(result.report),
                        _ => None,
                    })
                    .collect();
            }
            Mode::Analyze => {}
            Mode::Clean => self.analysis.fill(None),
        }
    }

    /// Ход очистки, если она начата после анализа.
    fn cleanup_progress(&self) -> Option<CleanupProgress> {
        let states = self.task_states.iter().map(|state| match state {
            TaskState::Idle => TaskProgress::NotStarted,
            TaskState::Running(report) => TaskProgress::Running(*report),
            TaskState::Done(result) => TaskProgress::Finished(result.report),
            TaskState::Skipped => TaskProgress::Finished(TaskReport::default()),
        });
        cleanup_progress(states.zip(self.expected.iter().copied()))
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

            // Сначала элементы справа, затем подзаголовок в оставшемся месте: на языках
            // с длинными надписями он обрежется многоточием, а не залезет под кнопки.
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

                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(Label::new(RichText::new(t!("header.subtitle")).weak()).truncate());
                });
            });
        });
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
                    ui.label(level.name()).on_hover_text(level.hint());
                    dot(ui, palette.safety(level));
                    ui.add_space(6.0);
                }
            });
        });
    }

    fn preset_selector(&mut self, ui: &mut Ui) {
        let active = presets::find_active(&self.tasks, &self.settings.presets, &self.current_selection());
        let selected_text = match &active {
            ActivePreset::BuiltIn(level) => presets::built_in_name(*level),
            ActivePreset::User(name) => name.clone(),
            ActivePreset::Custom => t!("presets.custom"),
        };

        let mut action = None;
        ComboBox::from_id_salt("presets").selected_text(selected_text).width(190.0).show_ui(ui, |ui| {
            let palette = Palette::of(ui);
            for level in Safety::ALL {
                let item = Button::selectable(
                    active == ActivePreset::BuiltIn(level),
                    (dot_atom(palette.safety(level)), presets::built_in_name(level)),
                );
                if ui.add(item).on_hover_text(presets::built_in_hint(level)).clicked() {
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
        let mut open_in_disk_tab = None;
        CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| self.task_row(ui, index, task))
            .body(|ui| {
                if let TaskState::Done(result) = &self.task_states[index] {
                    open_in_disk_tab = locations_grid(ui, task.id, &result.locations);
                }
            });
        if let Some(path) = open_in_disk_tab {
            self.disk_tab.open(path);
            self.tab = Tab::Disk;
        }
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
        let safety_line = t!("list.safety_line", level = task.safety.name(), hint = task.safety.hint());
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
        self.trash_policy_menu(&response, task);

        let result_color = match self.mode {
            Mode::Clean => palette.success,
            Mode::Analyze => palette.found,
        };
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Отступ под полосу прокрутки, чтобы она не закрывала цифры.
            ui.add_space(ui.spacing().scroll.bar_width + 4.0);
            if let Some(report) = self.restored.get(&index) {
                let color = if report.failed.is_empty() { result_color } else { palette.error };
                ui.label(RichText::new(restore_summary(report)).color(color));
                return;
            }
            self.undo_button(ui, index, task);
            Self::task_state_label(ui, &self.task_states[index], self.expected[index], result_color);
        });
    }

    /// Правый щелчок по пункту: что делать с его файлами — корзина или сразу удалить.
    fn trash_policy_menu(&mut self, response: &egui::Response, task: &CleanupTask) {
        if !task.deletes_files() || !self.settings.trash.is_enabled() {
            return;
        }
        let mut chosen = None;
        response.context_menu(|ui| {
            ui.label(RichText::new(t!("trash.policy.menu_title")).strong());
            let current = self.settings.trash.policy(task.id);
            for policy in [TrashPolicy::Normal, TrashPolicy::Always, TrashPolicy::Never] {
                if ui.radio(current == policy, trash_policy_name(policy)).clicked() {
                    chosen = Some(policy);
                    ui.close();
                }
            }
        });
        if let Some(policy) = chosen {
            self.settings.trash.set_policy(task.id, policy);
            self.settings.save();
        }
    }

    /// Круговая стрелка у пункта, файлы которого только что ушли в корзину: вернуть их.
    fn undo_button(&mut self, ui: &mut Ui, index: usize, task: &CleanupTask) {
        let TaskState::Done(result) = &self.task_states[index] else { return };
        let Some(run) = &self.undo_run else { return };
        if result.report.trashed_files == 0 {
            return;
        }
        if self.trash_worker.owner() == Some(JobOwner::Undo(index)) {
            ui.spinner();
            return;
        }
        let hint = t!("trash.undo_hint", files = result.report.trashed_files, size = format_size(result.report.trashed_bytes));
        let button = Button::new(undo_atom(16.0)).frame(false);
        let response = ui.add_enabled(!self.trash_worker.is_busy(), button).on_hover_text(&hint).on_disabled_hover_text(hint);
        if response.clicked() {
            let task_id = task.id.to_owned();
            let filter = Box::new(move |item: &hexcleaner::trash::TrashItem, _: &hexcleaner::trash::TrashFile| item.task == task_id);
            self.trash_worker.restore(JobOwner::Undo(index), self.settings.trash.directory(), run.clone(), filter);
        }
    }

    /// Диалог «Файлы не помещаются в корзину»: очистка стоит, пока не ответят.
    fn trash_limit_dialog(&mut self, ctx: &egui::Context) {
        let Some(request) = self.trash_limit else { return };
        let mut decision = None;
        Modal::new(Id::unique("trash_limit")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.heading(t!("trash.limit.title"));
            ui.label(t!(
                "trash.limit.text",
                used = format_size(request.used),
                limit = format_size(request.limit),
                needed = format_size(request.needed),
            ));
            ui.add_space(8.0);
            let options = [
                (TrashDecision::FreeSpace, t!("trash.limit.free_space"), t!("trash.limit.free_space.hint")),
                (TrashDecision::IgnoreLimit, t!("trash.limit.ignore"), t!("trash.limit.ignore.hint")),
                (TrashDecision::DeletePermanently, t!("trash.limit.delete"), t!("trash.limit.delete.hint")),
                (TrashDecision::Stop, t!("trash.limit.stop"), t!("trash.limit.stop.hint")),
            ];
            for (option, label, hint) in options {
                if ui.add(Button::new(label).min_size(vec2(ui.available_width(), 0.0))).on_hover_text(hint).clicked() {
                    decision = Some(option);
                }
            }
        });
        // Закрыть диалог, не ответив, нельзя: очистка ждёт решения.
        if let Some(decision) = decision {
            if let Some(job) = &self.job {
                job.answer_trash_limit(decision);
            }
            self.trash_limit = None;
        }
    }

    /// Итог пункта справа в строке: счётчики, пояснение или «пропущено».
    /// `expected` — итог анализа этого пункта, если очистка начата после анализа.
    fn task_state_label(ui: &mut Ui, state: &TaskState, expected: Option<TaskReport>, result_color: Color32) {
        match state {
            TaskState::Idle => {}
            TaskState::Running(report) => {
                let text = match expected {
                    Some(expected) => t!(
                        "progress.task",
                        files = report.deleted_files,
                        total = expected.deleted_files,
                        size = format_size(report.freed_bytes),
                    ),
                    None => report.summary(),
                };
                ui.label(text);
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
                if ui.add(Button::new(t!("footer.stop")).min_size(vec2(CLEAN_BUTTON_WIDTH, FOOTER_BUTTON_HEIGHT))).clicked() {
                    job.cancel();
                }
            } else {
                let has_selection = self
                    .tasks
                    .iter()
                    .any(|task| self.is_available(task) && self.is_selected(task));

                let analyze = Button::new(t!("footer.analyze")).min_size(vec2(110.0, FOOTER_BUTTON_HEIGHT));
                let response = ui.add_enabled(has_selection, analyze).on_hover_text(t!("footer.analyze.hint"));
                let can_start = has_selection && !self.is_running();
                if response.clicked() && can_start {
                    self.start(Mode::Analyze, Vec::new(), false);
                }

                // С зажатым Shift кнопка краснеет и удаляет мимо корзины.
                let trash_enabled = self.settings.trash.is_enabled();
                let bypass = trash_enabled && ui.input(|input| input.modifiers.shift);
                let size = vec2(CLEAN_BUTTON_WIDTH, FOOTER_BUTTON_HEIGHT);
                let (clean, hint) = if bypass {
                    let text = RichText::new(t!("footer.clean_permanently")).strong().color(Color32::WHITE);
                    let button = Button::new((warning_atom(16.0), text)).fill(Palette::of(ui).error).min_size(size);
                    (button, t!("footer.clean_permanently.hint"))
                } else {
                    let hint = if trash_enabled { t!("footer.clean.hint_shift") } else { t!("footer.clean.hint_no_trash") };
                    (Button::new(clean_label()).min_size(size), hint)
                };
                if ui.add_enabled(can_start, clean).on_hover_text(hint).clicked() {
                    self.bypass_trash = bypass;
                    self.check_programs_and_clean();
                }
            }

            if let Some(log_path) = &self.last_log
                && ui.add(side_button(ui, t!("footer.open_log"))).clicked()
            {
                open_in_explorer(log_path);
            }
            if ui.add(side_button(ui, t!("footer.logs_folder"))).clicked() {
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
                ui.label(RichText::new(t!("status.ready")).weak());
            }
            Status::CheckingPrograms | Status::ClosingPrograms | Status::PackingTrash => {
                let text = match self.status {
                    Status::CheckingPrograms => t!("status.checking_programs"),
                    Status::PackingTrash => t!("status.packing_trash"),
                    _ => t!("status.closing_programs"),
                };
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(text);
                });
            }
            Status::Running(Mode::Clean) if let Some(progress) = self.cleanup_progress() => {
                let text = t!(
                    "progress.overall",
                    percent = (progress.fraction * 100.0).round() as u32,
                    files = progress.deleted_files,
                    total = progress.expected_files,
                    size = format_size(progress.freed_bytes),
                    total_size = format_size(progress.expected_bytes),
                );
                ui.add(ProgressBar::new(progress.fraction).text(text).animate(true));
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
                // Перенесённое в корзину место на диске пока не освободило.
                if total.trashed_bytes > 0 {
                    text += &t!("status.trashed", size = format_size(total.trashed_bytes));
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
    /// Вызывается и тогда, когда окно свёрнуто (а `ui` — нет): поэтому события очистки
    /// обрабатываются здесь — иначе о завершении программа узнала бы, только когда окно развернут.
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.window.is_none() {
            self.window = window_handle(frame);
        }
        self.process_cleanup_events(ctx);
        self.poll_programs_check(ctx);
        if let Some((owner, result)) = self.trash_worker.poll(ctx) {
            match (owner, result) {
                (JobOwner::Undo(index), JobResult::Restored(report)) => {
                    self.restored.insert(index, report);
                    self.trash_tab.mark_dirty();
                }
                (_, JobResult::Removed(report)) => {
                    self.disk_tab.removal_finished(report);
                    self.trash_tab.mark_dirty();
                }
                (_, result) => self.trash_tab.job_finished(result),
            }
        }
        if self.job.is_none() {
            self.trash_limit = None;
        }
        if let Some(exe) = self.updater.poll(ctx, &mut self.settings) {
            self.restart_after_update(ctx, &exe);
        }
    }

    /// Боковые кнопки мыши — «Назад» и «Вперёд». egui считает их обычными кнопками и начинает
    /// ими выделять текст в подписях, поэтому их нажатия забираем себе до того, как он их увидит.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        raw_input.events.retain(|event| match event {
            egui::Event::PointerButton { button: button @ (PointerButton::Extra1 | PointerButton::Extra2), pressed, .. } => {
                if *pressed {
                    self.side_buttons.push(*button == PointerButton::Extra2);
                }
                false
            }
            _ => true,
        });
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        for forward in std::mem::take(&mut self.side_buttons) {
            // Назад и вперёд пока есть только на вкладке «Место на диске».
            match (self.tab, forward) {
                (Tab::Disk, false) => self.disk_tab.go_back(),
                (Tab::Disk, true) => self.disk_tab.go_forward(),
                _ => {}
            }
        }

        self.programs_dialog(ui.ctx());
        self.trash_limit_dialog(ui.ctx());
        // Предложение обновиться не перекрывает диалог закрытия программ.
        if matches!(self.programs_check, ProgramsCheck::Idle) {
            let busy = self.is_cleaning();
            self.updater.dialog(ui.ctx(), &mut self.settings, busy);
        }

        Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            self.header(ui);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Cleanup, t!("tabs.cleanup"));
                ui.selectable_value(&mut self.tab, Tab::Disk, t!("tabs.disk"));
                ui.selectable_value(&mut self.tab, Tab::Trash, t!("tabs.trash"));
                ui.selectable_value(&mut self.tab, Tab::Schedule, t!("tabs.schedule"));
                ui.selectable_value(&mut self.tab, Tab::Settings, t!("tabs.settings"));
            });
            ui.add_space(4.0);
        });

        match self.tab {
            Tab::Cleanup => {
                Panel::bottom("footer").show(ui, |ui| self.footer(ui));
                CentralPanel::default().show(ui, |ui| self.task_list(ui));
            }
            Tab::Schedule => {
                CentralPanel::default().show(ui, |ui| {
                    self.schedule_tab.ui(ui, &mut self.settings, &self.tasks, self.is_admin);
                });
            }
            Tab::Trash => {
                CentralPanel::default().show(ui, |ui| self.trash_tab.ui(ui, &self.settings, &mut self.trash_worker));
            }
            Tab::Disk => {
                let window = self.window;
                CentralPanel::default().show(ui, |ui| {
                    self.disk_tab.ui(ui, window, &self.settings, &mut self.trash_worker);
                });
            }
            Tab::Settings => {
                let busy = self.is_running();
                CentralPanel::default().show(ui, |ui| {
                    let (tasks, window) = (Arc::clone(&self.tasks), self.window);
                    self.settings_tab.ui(ui, &mut self.settings, &tasks, &mut self.updater, self.is_admin, busy, window);
                });
            }
        }
    }
}

fn clean_label() -> RichText {
    RichText::new(t!("footer.clean")).strong()
}

/// Кнопка «Открыть журнал» или «Папка журналов»: той же высоты, что «Начать очистку»,
/// и с тем же отступом текста от краёв.
fn side_button(ui: &Ui, text: String) -> Button<'static> {
    let text_width = |text: WidgetText| {
        text.into_galley(ui, Some(TextWrapMode::Extend), f32::INFINITY, TextStyle::Button).size().x
    };
    let padding = ((CLEAN_BUTTON_WIDTH - text_width(clean_label().into())) / 2.0).max(ui.spacing().button_padding.x);
    let width = text_width(text.clone().into()) + 2.0 * padding;
    Button::new(text).min_size(vec2(width, FOOTER_BUTTON_HEIGHT))
}

/// Таблица «объём — число файлов — папка» для раскрытого пункта.
/// Возвращает папку, которую выбрали открыть во вкладке «Место на диске».
fn locations_grid(ui: &mut Ui, task_id: &str, locations: &[Location]) -> Option<PathBuf> {
    let mut open_in_disk_tab = None;
    Grid::new(("locations", task_id)).num_columns(3).spacing([16.0, 2.0]).show(ui, |ui| {
        for location in locations.iter().take(MAX_SHOWN_LOCATIONS) {
            ui.label(RichText::new(format_size(location.bytes)).strong());
            ui.label(t!("locations.files", count = location.files));
            let hint = t!("locations.open_hint", path = location.path.display());
            // По щелчку — меню: открыть во вкладке «Место на диске» или в проводнике.
            let link = ui.link(short_path(&location.path)).on_hover_text(hint);
            Popup::menu(&link).show(|ui| {
                if ui.button(t!("locations.open_in_disk_tab")).clicked() {
                    open_in_disk_tab = Some(location.path.clone());
                }
                if ui.button(t!("locations.open_in_explorer")).clicked() {
                    open_in_explorer(&location.path);
                }
            });
            ui.end_row();
        }
    });

    let hidden = locations.len().saturating_sub(MAX_SHOWN_LOCATIONS);
    if hidden > 0 {
        ui.label(RichText::new(t!("locations.more", count = hidden)).weak());
    }
    open_in_disk_tab
}

/// Что делать с файлами пункта — на языке интерфейса.
pub fn trash_policy_name(policy: TrashPolicy) -> String {
    match policy {
        TrashPolicy::Normal => t!("trash.policy.normal"),
        TrashPolicy::Always => t!("trash.policy.always"),
        TrashPolicy::Never => t!("trash.policy.never"),
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

/// Активно ли окно. Свёрнутое окно тоже неактивно.
fn is_focused(ctx: &egui::Context) -> bool {
    ctx.input(|input| {
        let viewport = input.viewport();
        viewport.focused.unwrap_or(true) && !viewport.minimized.unwrap_or(false)
    })
}

/// Дескриптор окна Windows (HWND) — для уведомлений.
fn window_handle(frame: &eframe::Frame) -> Option<isize> {
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// Заголовок окна на текущем языке.
pub fn window_title(is_admin: bool) -> String {
    if is_admin { t!("window.title_admin") } else { t!("window.title") }
}

/// Открывает файл в программе по умолчанию или папку в проводнике.
pub fn open_in_explorer(path: &Path) {
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
