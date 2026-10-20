//! Обновление из окна: проверка при запуске и вручную, диалог «Доступна новая версия»
//! и загрузка с установкой. Вся работа с сетью и файлами — в `hexcleaner::update`,
//! здесь — потоки и интерфейс.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use chrono::Local;
use eframe::egui::{self, Button, Id, Modal, ProgressBar, RichText, ScrollArea, Ui};
use hexcleaner::cleaner::format_size;
use hexcleaner::settings::Settings;
use hexcleaner::t;
use hexcleaner::update::{self, CheckResult, Postpone, Release};

/// Через сколько дней можно напомнить об обновлении.
const POSTPONE_DAYS: [u64; 3] = [1, 3, 7];

pub struct Updater {
    /// Путь к запущенной программе. Запоминается при запуске: после установки
    /// по нему запускается новая версия.
    exe: Option<PathBuf>,
    /// Найденная версия новее этой.
    latest: Option<Release>,
    task: Task,
    /// Итог последней проверки или установки — для вкладки «Настройки».
    outcome: Option<Outcome>,
    /// Показан ли диалог с предложением обновиться.
    dialog_open: bool,
}

enum Task {
    None,
    /// `manual` — проверку запустил пользователь: тогда найденную версию предлагаем,
    /// даже если её решили пропустить.
    Checking { manual: bool, receiver: Receiver<Result<CheckResult, String>> },
    Downloading { progress: Arc<DownloadProgress>, receiver: Receiver<Result<(), String>> },
}

enum Outcome {
    UpToDate,
    /// Эта сборка новее последнего релиза `latest`.
    Ahead { latest: String },
    /// На GitHub нет опубликованных релизов (только черновики или предварительные выпуски).
    NoReleases,
    CheckFailed(String),
    InstallFailed(String),
}

#[derive(Default)]
struct DownloadProgress {
    downloaded: AtomicU64,
    /// 0 — размер неизвестен.
    total: AtomicU64,
    cancelled: AtomicBool,
}

impl Updater {
    pub fn new() -> Self {
        let exe = std::env::current_exe().ok();
        if let Some(exe) = &exe {
            update::remove_leftovers(exe);
        }
        Self { exe, latest: None, task: Task::None, outcome: None, dialog_open: false }
    }

    /// Проверка при запуске, если она включена, не отложена и с прошлой прошло достаточно времени.
    pub fn check_on_start(&mut self, settings: &Settings) {
        if settings.updates.should_check_on_start(Local::now()) {
            self.start_check(false);
        }
    }

    /// Идёт загрузка или установка: в это время нельзя начинать очистку — по окончании
    /// программа перезапустится.
    pub fn is_installing(&self) -> bool {
        matches!(self.task, Task::Downloading { .. })
    }

    fn start_check(&mut self, manual: bool) {
        if !matches!(self.task, Task::None) {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(update::check().map_err(|error| error.to_string()));
        });
        self.task = Task::Checking { manual, receiver };
        self.outcome = None;
    }

    fn start_download(&mut self) {
        let (Some(release), Some(exe)) = (self.latest.clone(), self.exe.clone()) else { return };
        let progress = Arc::new(DownloadProgress::default());
        let (sender, receiver) = mpsc::channel();
        let thread_progress = Arc::clone(&progress);
        thread::spawn(move || {
            let result = update::download_and_install(&release, &exe, |downloaded, total| {
                thread_progress.downloaded.store(downloaded, Ordering::Relaxed);
                thread_progress.total.store(total.unwrap_or(0), Ordering::Relaxed);
                !thread_progress.cancelled.load(Ordering::Relaxed)
            });
            let _ = sender.send(result.map_err(|error| error.to_string()));
        });
        self.task = Task::Downloading { progress, receiver };
        self.outcome = None;
    }

    /// Обрабатывает результаты фоновой работы. Возвращает путь к программе,
    /// если новая версия установлена и её пора запустить.
    pub fn poll(&mut self, ctx: &egui::Context, settings: &mut Settings) -> Option<PathBuf> {
        match &self.task {
            Task::None => None,
            Task::Checking { manual, receiver } => {
                let manual = *manual;
                let result = match receiver.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => {
                        ctx.request_repaint_after(Duration::from_millis(100));
                        return None;
                    }
                    Err(TryRecvError::Disconnected) => Err(t!("update.error.invalid_response")),
                };
                self.task = Task::None;
                if result.is_ok() {
                    settings.updates.last_check = Some(Local::now());
                    settings.save();
                }
                // Найденный раньше релиз могли снять с публикации.
                self.latest = None;
                match result {
                    Ok(CheckResult::Available(release)) => {
                        self.dialog_open = manual || settings.updates.should_offer(&release);
                        self.latest = Some(release);
                    }
                    Ok(CheckResult::UpToDate) => self.outcome = Some(Outcome::UpToDate),
                    Ok(CheckResult::Ahead { latest }) => self.outcome = Some(Outcome::Ahead { latest }),
                    Ok(CheckResult::NoReleases) => self.outcome = Some(Outcome::NoReleases),
                    // При запуске об ошибке не сообщаем: её видно во вкладке «Настройки».
                    Err(error) => self.outcome = Some(Outcome::CheckFailed(error)),
                }
                None
            }
            Task::Downloading { receiver, .. } => {
                let result = match receiver.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => {
                        ctx.request_repaint_after(Duration::from_millis(100));
                        return None;
                    }
                    Err(TryRecvError::Disconnected) => Err(t!("update.error.cancelled")),
                };
                self.task = Task::None;
                match result {
                    Ok(()) => self.exe.clone(),
                    Err(error) => {
                        self.outcome = Some(Outcome::InstallFailed(error));
                        None
                    }
                }
            }
        }
    }

    /// Диалог «Доступна новая версия». `busy` — идёт очистка: обновляться нельзя.
    pub fn dialog(&mut self, ctx: &egui::Context, settings: &mut Settings, busy: bool) {
        if !self.dialog_open {
            return;
        }
        let Some(release) = self.latest.clone() else { return };
        let mut update_now = false;
        let mut postpone = None;
        let mut close = false;

        let modal = Modal::new(Id::unique("update_dialog")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(t!("update.dialog.title", version = &release.version));
            ui.label(t!("update.dialog.current", version = update::CURRENT_VERSION));
            ui.hyperlink_to(t!("update.release_page"), &release.page_url);

            if !release.notes.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(t!("update.dialog.notes")).strong());
                ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    ui.label(&release.notes);
                });
            }
            ui.separator();

            if let Task::Downloading { progress, .. } = &self.task {
                download_progress(ui, progress);
                if ui.button(t!("update.cancel")).clicked() {
                    progress.cancelled.store(true, Ordering::Relaxed);
                }
                return;
            }
            if let Some(Outcome::InstallFailed(error)) = &self.outcome {
                ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
                ui.add_space(4.0);
            }

            ui.horizontal(|ui| {
                let label = t!("update.dialog.update", size = format_size(release.size));
                let button = Button::new(RichText::new(label).strong());
                update_now = ui
                    .add_enabled(!busy, button)
                    .on_disabled_hover_text(t!("update.busy"))
                    .on_hover_text(t!("update.dialog.update.hint"))
                    .clicked();
                if ui.button(t!("update.dialog.close")).clicked() {
                    close = true;
                }
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(t!("update.dialog.remind"));
                for days in POSTPONE_DAYS {
                    if ui.button(postpone_label(days)).clicked() {
                        postpone = Some(Postpone::Days(days));
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button(t!("update.dialog.skip_version")).clicked() {
                    postpone = Some(Postpone::SkipVersion);
                }
                let disable = ui.button(t!("update.dialog.disable")).on_hover_text(t!("update.dialog.disable.hint"));
                if disable.clicked() {
                    postpone = Some(Postpone::DisableChecks);
                }
            });
        });

        // Закрыть диалог во время загрузки нельзя — её можно только остановить.
        if modal.should_close() && !self.is_installing() {
            close = true;
        }
        if update_now {
            self.start_download();
        } else if let Some(choice) = postpone {
            settings.updates.postpone(choice, &release, Local::now());
            settings.save();
            self.dialog_open = false;
        } else if close {
            self.dialog_open = false;
        }
    }

    /// Раздел «Обновления» во вкладке «Настройки».
    pub fn settings_section(&mut self, ui: &mut Ui, settings: &mut Settings) {
        ui.label(t!("update.current_version", version = update::CURRENT_VERSION));

        let check = ui.checkbox(&mut settings.updates.check_on_start, t!("update.check_on_start"));
        if check.on_hover_text(t!("update.check_on_start.hint")).changed() {
            settings.save();
        }

        ui.horizontal(|ui| {
            // Вручную можно проверить в любой момент; ограничение по времени — только при запуске.
            let idle = matches!(self.task, Task::None);
            if ui.add_enabled(idle, Button::new(t!("update.check_now"))).clicked() {
                self.start_check(true);
            }
            match (&self.task, &self.outcome, &self.latest) {
                (Task::Checking { .. }, _, _) => {
                    ui.spinner();
                    ui.label(t!("update.checking"));
                }
                (Task::Downloading { .. }, _, _) => {
                    ui.spinner();
                    ui.label(t!("update.downloading"));
                }
                (_, _, Some(release)) => {
                    ui.label(RichText::new(t!("update.available", version = &release.version)).strong());
                    if ui.button(t!("update.show")).clicked() {
                        self.dialog_open = true;
                    }
                }
                (_, Some(Outcome::UpToDate), None) => {
                    ui.label(t!("update.up_to_date"));
                }
                (_, Some(Outcome::Ahead { latest }), None) => {
                    let text = t!("update.ahead", version = update::CURRENT_VERSION, latest = latest);
                    ui.label(text);
                }
                (_, Some(Outcome::NoReleases), None) => {
                    ui.label(t!("update.no_releases"));
                }
                (_, Some(Outcome::CheckFailed(error) | Outcome::InstallFailed(error)), None) => {
                    ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
                }
                (_, None, None) => {}
            }
        });

        if let Some(last) = settings.updates.last_check {
            let last = last.format("%d.%m.%Y %H:%M");
            ui.label(RichText::new(t!("update.last_check", date = last)).weak());
        }

        // Отложенные напоминания и пропущенную версию можно отменить.
        let now = Local::now();
        let mut reset = false;
        if let Some(until) = settings.updates.postponed_until.filter(|until| *until > now) {
            ui.horizontal(|ui| {
                let until = until.format("%d.%m.%Y %H:%M");
                ui.label(RichText::new(t!("update.postponed_until", date = until)).weak());
                reset |= ui.small_button(t!("update.reset")).clicked();
            });
        }
        if let Some(version) = &settings.updates.skipped_version {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t!("update.skipped", version = version)).weak());
                reset |= ui.small_button(t!("update.reset")).clicked();
            });
        }
        if reset {
            settings.updates.postponed_until = None;
            settings.updates.skipped_version = None;
            settings.save();
        }
    }
}

fn download_progress(ui: &mut Ui, progress: &DownloadProgress) {
    let downloaded = progress.downloaded.load(Ordering::Relaxed);
    let total = progress.total.load(Ordering::Relaxed);
    let (fraction, text) = if total > 0 {
        let text = t!("update.progress", size = format_size(downloaded), total = format_size(total));
        (downloaded as f32 / total as f32, text)
    } else {
        (0.0, t!("update.progress_unknown", size = format_size(downloaded)))
    };
    ui.add(ProgressBar::new(fraction).text(text).animate(true));
}

fn postpone_label(days: u64) -> String {
    match days {
        1 => t!("update.dialog.remind_day"),
        7 => t!("update.dialog.remind_week"),
        _ => t!("update.dialog.remind_days", count = days),
    }
}
