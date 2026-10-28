//! Вкладка «Корзина» и фоновые операции с корзиной: восстановление и удаление.
//!
//! Восстанавливать можно очистку целиком, пункт, папку или отдельный файл. Операции
//! выполняются по одной в фоновом потоке ([`TrashWorker`]): описание очистки читается
//! и переписывается целиком, поэтому две операции одновременно могли бы потерять изменения.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Local, TimeDelta};
use eframe::egui::collapsing_header::CollapsingState;
use eframe::egui::{self, Align, Button, Id, Layout, Modal, RichText, ScrollArea, Ui};
use hexcleaner::cleaner::format_size;
use hexcleaner::settings::Settings;
use hexcleaner::t;
use hexcleaner::trash::{
    RemoveReport, RestoreReport, Trash, TrashDecision, TrashFile, TrashItem, TrashRun, TrashSettings, remove_path,
};

use crate::app::open_in_explorer;
use crate::widgets::elided_label;

/// Сколько файлов папки показывать в списке; остальные восстанавливаются вместе с папкой.
const MAX_SHOWN_FILES: usize = 200;

/// Кто запустил операцию — туда и придёт результат.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOwner {
    /// Кнопка отмены у пункта на вкладке «Очистка» (индекс пункта).
    Undo(usize),
    TrashTab,
    /// Удаление файла или папки на вкладке «Место на диске».
    DiskTab,
}

pub enum JobResult {
    Restored(RestoreReport),
    Deleted(Result<(), String>),
    Removed(RemoveReport),
}

/// Что восстановить.
type Filter = Box<dyn Fn(&TrashItem, &TrashFile) -> bool + Send>;

/// Фоновые операции с корзиной, по одной за раз.
#[derive(Default)]
pub struct TrashWorker {
    job: Option<(JobOwner, Receiver<JobResult>)>,
}

impl TrashWorker {
    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn owner(&self) -> Option<JobOwner> {
        self.job.as_ref().map(|(owner, _)| *owner)
    }

    /// Возвращает на место файлы очистки `run`, выбранные `filter`.
    pub fn restore(&mut self, owner: JobOwner, dir: PathBuf, run: String, filter: Filter) {
        self.spawn(owner, move || JobResult::Restored(Trash::new(dir).restore(&run, filter)));
    }

    /// Удаляет файл или папку `path`: с `trash` — в корзину (`decision` — если не хватит лимита),
    /// без неё — навсегда. `title` — под каким названием удалённое видно в корзине.
    pub fn remove(
        &mut self,
        owner: JobOwner,
        path: PathBuf,
        title: String,
        trash: Option<TrashSettings>,
        decision: TrashDecision,
    ) {
        self.spawn(owner, move || JobResult::Removed(remove_path(&path, &title, trash.as_ref(), decision)));
    }

    /// Удаляет очистку `run` из корзины навсегда; `None` — всю корзину.
    fn delete(&mut self, owner: JobOwner, dir: PathBuf, run: Option<String>) {
        self.spawn(owner, move || {
            let trash = Trash::new(dir);
            let result = match run {
                Some(run) => trash.delete_run(&run),
                None => trash.empty(),
            };
            JobResult::Deleted(result.map_err(|error| error.to_string()))
        });
    }

    fn spawn(&mut self, owner: JobOwner, work: impl FnOnce() -> JobResult + Send + 'static) {
        if self.is_busy() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(work());
        });
        self.job = Some((owner, receiver));
    }

    /// Результат законченной операции.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<(JobOwner, JobResult)> {
        let (owner, receiver) = self.job.as_ref()?;
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(100));
                return None;
            }
            Err(TryRecvError::Disconnected) => JobResult::Deleted(Err(t!("trash.error.worker"))),
        };
        let owner = *owner;
        self.job = None;
        Some((owner, result))
    }
}

/// Текст итога восстановления: «Восстановлено файлов: 12 (3.4 МБ)», плюс пропущенные и ошибки.
pub fn restore_summary(report: &RestoreReport) -> String {
    let mut text = t!("trash.restored", files = report.restored, size = format_size(report.restored_bytes));
    if report.existing > 0 {
        text += &t!("trash.restored_existing", count = report.existing);
    }
    if !report.failed.is_empty() {
        text += &t!("trash.restored_failed", count = report.failed.len());
    }
    text
}

/// Файлы одной папки внутри пункта (номера файлов в `TrashItem::files`).
struct FolderGroup {
    dir: PathBuf,
    files: Vec<usize>,
    size: u64,
}

#[derive(Default)]
pub struct TrashTab {
    runs: Vec<TrashRun>,
    /// Папка, из которой прочитан список; другая папка в настройках — перечитать.
    loaded_dir: Option<PathBuf>,
    dirty: bool,
    /// Итог последней операции; `true` — ошибка.
    message: Option<(String, bool)>,
    /// Файлы пунктов по папкам (ключ — очистка и пункт); строится при первом раскрытии пункта.
    groups: HashMap<(String, String), Vec<FolderGroup>>,
    confirm: Option<Confirm>,
}

/// Удаление навсегда — после подтверждения.
enum Confirm {
    EmptyAll,
    DeleteRun(String),
}

impl TrashTab {
    /// Перечитать корзину при следующей отрисовке (после очистки или восстановления).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Операция, запущенная с этой вкладки, закончилась.
    pub fn job_finished(&mut self, result: JobResult) {
        self.message = Some(match result {
            JobResult::Restored(report) => (restore_summary(&report), !report.failed.is_empty()),
            JobResult::Deleted(Ok(())) => (t!("trash.deleted"), false),
            JobResult::Deleted(Err(error)) => (t!("trash.error.delete", error = error), true),
            JobResult::Removed(_) => return,
        });
        self.mark_dirty();
    }

    fn reload(&mut self, dir: &Path) {
        self.runs = Trash::new(dir.to_owned()).runs();
        self.loaded_dir = Some(dir.to_owned());
        self.groups.clear();
        self.dirty = false;
    }

    pub fn ui(&mut self, ui: &mut Ui, settings: &Settings, worker: &mut TrashWorker) {
        let dir = settings.trash.directory();
        if self.dirty || self.loaded_dir.as_deref() != Some(dir.as_path()) {
            self.reload(&dir);
        }

        let busy = worker.is_busy();
        let used: u64 = self.runs.iter().map(|run| run.stored_bytes).sum();
        let limit = match settings.trash.limit {
            Some(0) => t!("trash.limit_disabled"),
            Some(limit) => format_size(limit),
            None => t!("trash.limit_unlimited"),
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(t!("trash.used", used = format_size(used), limit = limit)).strong());
            ui.separator();
            if ui.button(t!("trash.refresh")).clicked() {
                self.mark_dirty();
            }
            if ui.button(t!("trash.open_folder")).on_hover_text(dir.display().to_string()).clicked() {
                let _ = std::fs::create_dir_all(&dir);
                open_in_explorer(&dir);
            }
            let empty = ui.add_enabled(!busy && !self.runs.is_empty(), Button::new(t!("trash.empty")));
            if empty.clicked() {
                self.confirm = Some(Confirm::EmptyAll);
            }
        });
        if !settings.trash.is_enabled() {
            ui.label(RichText::new(t!("trash.disabled_hint")).weak());
        }
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(t!("trash.working"));
            });
        } else if let Some((message, is_error)) = &self.message {
            let color = if *is_error { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
            ui.label(RichText::new(message).color(color));
        }
        ui.separator();

        if self.runs.is_empty() {
            ui.label(RichText::new(t!("trash.nothing")).weak());
        }
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for run_index in 0..self.runs.len() {
                self.run_entry(ui, run_index, &dir, busy, worker, settings.trash.keep_days);
            }
        });

        self.confirmation(ui.ctx(), &dir, worker);
    }

    /// `keep_days` — срок хранения очисток: рядом с очисткой пишется, сколько ей осталось.
    fn run_entry(
        &mut self,
        ui: &mut Ui,
        run_index: usize,
        dir: &Path,
        busy: bool,
        worker: &mut TrashWorker,
        keep_days: Option<u32>,
    ) {
        let run = &self.runs[run_index];
        let id = run.id.clone();
        let mut header = t!(
            "trash.run",
            date = run.created.format("%d.%m.%Y %H:%M:%S"),
            size = format_size(run.total_size()),
            files = run.file_count(),
        );
        if run.archived {
            header += &t!("trash.run_zip", size = format_size(run.stored_bytes));
        }
        if let Some(days) = keep_days {
            header += &expiry_text(run.created, days, Local::now());
        }

        CollapsingState::load_with_default_open(ui.ctx(), ui.make_persistent_id(("trash_run", &id)), false)
            .show_header(ui, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Справа налево: первая кнопка — крайняя справа.
                    if ui.add_enabled(!busy, Button::new(t!("trash.delete_run")).small()).clicked() {
                        self.confirm = Some(Confirm::DeleteRun(id.clone()));
                    }
                    if ui.add_enabled(!busy, Button::new(t!("trash.restore_run")).small()).clicked() {
                        worker.restore(JobOwner::TrashTab, dir.to_owned(), id.clone(), Box::new(|_, _| true));
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| elided_label(ui, header, true));
                });
            })
            .body(|ui| {
                for item_index in 0..self.runs[run_index].items.len() {
                    self.item_entry(ui, run_index, item_index, dir, busy, worker);
                }
            });
    }

    fn item_entry(&mut self, ui: &mut Ui, run_index: usize, item_index: usize, dir: &Path, busy: bool, worker: &mut TrashWorker) {
        let run_id = self.runs[run_index].id.clone();
        let item = &self.runs[run_index].items[item_index];
        let task = item.task.clone();
        let header = t!("trash.item", title = &item.title, size = format_size(item.total_size()), files = item.files.len());

        CollapsingState::load_with_default_open(ui.ctx(), ui.make_persistent_id(("trash_item", &run_id, &task)), false)
            .show_header(ui, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(!busy, Button::new(t!("trash.restore_item")).small()).clicked() {
                        let task = task.clone();
                        let filter: Filter = Box::new(move |item, _| item.task == task);
                        worker.restore(JobOwner::TrashTab, dir.to_owned(), run_id.clone(), filter);
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| elided_label(ui, header, false));
                });
            })
            .body(|ui| {
                let item = &self.runs[run_index].items[item_index];
                let groups = self.groups.entry((run_id.clone(), task.clone())).or_insert_with(|| folder_groups(item));
                for group in groups.iter() {
                    folder_entry(ui, &run_id, item, group, dir, busy, worker);
                }
            });
    }

    /// Подтверждение удаления навсегда.
    fn confirmation(&mut self, ctx: &egui::Context, dir: &Path, worker: &mut TrashWorker) {
        let Some(confirm) = &self.confirm else { return };
        let question = match confirm {
            Confirm::EmptyAll => t!("trash.confirm_empty"),
            Confirm::DeleteRun(id) => t!("trash.confirm_delete_run", run = id),
        };
        let (mut yes, mut no) = (false, false);
        let modal = Modal::new(Id::unique("trash_confirm")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(question);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                yes = ui.button(RichText::new(t!("trash.confirm_yes")).strong()).clicked();
                no = ui.button(t!("trash.confirm_no")).clicked();
            });
        });
        if yes {
            let run = match confirm {
                Confirm::EmptyAll => None,
                Confirm::DeleteRun(id) => Some(id.clone()),
            };
            worker.delete(JobOwner::TrashTab, dir.to_owned(), run);
        }
        if yes || no || modal.should_close() {
            self.confirm = None;
        }
    }
}

/// Папка внутри пункта: её файлы можно вернуть все сразу или по одному.
fn folder_entry(
    ui: &mut Ui,
    run_id: &str,
    item: &TrashItem,
    group: &FolderGroup,
    dir: &Path,
    busy: bool,
    worker: &mut TrashWorker,
) {
    let header = t!("trash.folder", path = group.dir.display(), size = format_size(group.size), files = group.files.len());
    let id = ui.make_persistent_id(("trash_folder", run_id, &item.task, &group.dir));
    CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(!busy, Button::new(t!("trash.restore_folder")).small()).clicked() {
                    let (task, folder) = (item.task.clone(), group.dir.clone());
                    let filter: Filter =
                        Box::new(move |item, file| item.task == task && file.path.parent() == Some(&folder));
                    worker.restore(JobOwner::TrashTab, dir.to_owned(), run_id.to_owned(), filter);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| elided_label(ui, header, false));
            });
        })
        .body(|ui| {
            for &index in group.files.iter().take(MAX_SHOWN_FILES) {
                let file = &item.files[index];
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add_enabled(!busy, Button::new(t!("trash.restore_file")).small()).clicked() {
                            let (task, path) = (item.task.clone(), file.path.clone());
                            let filter: Filter = Box::new(move |item, file| item.task == task && file.path == path);
                            worker.restore(JobOwner::TrashTab, dir.to_owned(), run_id.to_owned(), filter);
                        }
                        ui.label(RichText::new(format_size(file.size)).weak());
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let name = file.path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                            elided_label(ui, name, false).on_hover_text(file.path.display().to_string());
                        });
                    });
                });
            }
            let hidden = group.files.len().saturating_sub(MAX_SHOWN_FILES);
            if hidden > 0 {
                ui.label(RichText::new(t!("trash.more_files", count = hidden)).weak());
            }
        });
}

/// « (до удаления: 5 дн.)» — сколько очистке осталось храниться в корзине.
/// Срок уже вышел — она удалится при следующем запуске или очистке.
fn expiry_text(created: DateTime<Local>, keep_days: u32, now: DateTime<Local>) -> String {
    let left = created + TimeDelta::days(i64::from(keep_days)) - now;
    let time = if left >= TimeDelta::days(1) {
        t!("trash.expires_days", count = left.num_days())
    } else if left > TimeDelta::zero() {
        // Неполный час считаем за час: «0 ч» выглядело бы как «уже удалена».
        t!("trash.expires_hours", count = (left.num_minutes() + 59) / 60)
    } else {
        return t!("trash.expires_soon");
    };
    t!("trash.expires_in", time = time)
}

/// Файлы пункта по папкам, самые большие папки — первыми.
fn folder_groups(item: &TrashItem) -> Vec<FolderGroup> {
    let mut groups: BTreeMap<PathBuf, FolderGroup> = BTreeMap::new();
    for (index, file) in item.files.iter().enumerate() {
        let dir = file.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let group = groups.entry(dir.clone()).or_insert_with(|| FolderGroup { dir, files: Vec::new(), size: 0 });
        group.files.push(index);
        group.size += file.size;
    }
    let mut groups: Vec<FolderGroup> = groups.into_values().collect();
    groups.sort_by_key(|group| std::cmp::Reverse(group.size));
    for group in &mut groups {
        group.files.sort_by_key(|&index| std::cmp::Reverse(item.files[index].size));
    }
    groups
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn expiry_shows_days_then_hours() {
        hexcleaner::i18n::set_language("en").unwrap();
        let created = Local.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap();
        let at = |days: i64, hours: i64| created + TimeDelta::days(days) + TimeDelta::hours(hours);
        assert_eq!(expiry_text(created, 7, at(1, 0)), " (6 d until deletion)");
        assert_eq!(expiry_text(created, 7, at(6, 1)), " (23 h until deletion)");
        assert_eq!(expiry_text(created, 7, at(6, 23) + TimeDelta::minutes(30)), " (1 h until deletion)");
        assert_eq!(expiry_text(created, 7, at(7, 1)), " (will be deleted at the next start or cleanup)");
    }
}
