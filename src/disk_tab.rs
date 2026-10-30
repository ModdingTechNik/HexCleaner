//! Вкладка «Место на диске»: что занимает место в папке или на диске.
//!
//! Слева — содержимое открытой папки от больших элементов к меньшим, справа — кольцевая
//! диаграмма. Мелкие элементы объединяются в сектор «Остальное», который можно раскрыть.
//! Сканирование — в `hexcleaner::disk_usage`.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui::{
    self, Align, Align2, Button, Color32, ComboBox, FontId, Id, Key, Label, Layout, Mesh, Modal, Pos2, Rect,
    Response, RichText, ScrollArea, Sense, Shape, Stroke, TextEdit, Ui, vec2,
};
use hexcleaner::cleaner::{format_size, is_protected};
use hexcleaner::disk_usage::{self, DirKind, DirNode, Entry, EntryKind, ScanJob, Slice};
use hexcleaner::settings::Settings;
use hexcleaner::system::drives::{self, Drive};
use hexcleaner::t;
use hexcleaner::trash::{RemoveReport, Trash, TrashDecision};

use crate::app::open_in_explorer;
use crate::theme::Palette;
use crate::trash_tab::{JobOwner, TrashWorker};
use crate::widgets::warning_atom;

/// Элементы меньше этой доли объединяются в сектор «Остальное».
const MIN_SLICE_SHARE: f64 = 0.03;

/// Ширина диаграммы справа от списка.
const CHART_WIDTH: f32 = 250.0;

/// Высота строки списка.
const ROW_HEIGHT: f32 = 22.0;

#[derive(Default)]
pub struct DiskTab {
    drives: Vec<Drive>,
    drives_loaded: bool,
    /// Путь в поле ввода.
    path_input: String,
    scan: Option<ScanJob>,
    result: Option<ScanResult>,
    /// Открытая папка: имена папок от корня сканирования.
    current: Vec<String>,
    /// Содержимое открытой папки.
    entries: Vec<Entry>,
    /// Раскрытые секторы «Остальное»: номера элементов на каждом уровне раскрытия.
    chart_stack: Vec<Vec<usize>>,
    /// Папки для «Назад» и «Вперёд» (кнопки и боковые кнопки мыши), последняя — ближайшая.
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    error: Option<String>,
    /// Удаление ждёт подтверждения.
    delete_request: Option<DeleteRequest>,
    /// Идёт удаление: по окончании его итог вычитается из дерева.
    deleting: Option<DeleteRequest>,
    /// Итог последнего удаления; `true` — были ошибки.
    delete_message: Option<(String, bool)>,
}

/// Что удалить и как.
#[derive(Clone)]
struct DeleteRequest {
    path: PathBuf,
    /// Папка, в которой лежит удаляемое: имена папок от корня сканирования.
    parent: Vec<String>,
    name: String,
    is_dir: bool,
    size: u64,
    files: u64,
    /// Мимо корзины.
    permanent: bool,
    /// Сколько занято в корзине и её лимит — чтобы заранее предупредить о нехватке места.
    trash_used: u64,
    trash_limit: Option<u64>,
}

/// Что строкам списка нужно знать для пункта «Удалить» в меню.
struct DeleteContext {
    /// Корзина включена: без Shift удаление идёт в неё.
    trash_enabled: bool,
    trash_dir: PathBuf,
    /// Другая операция с корзиной ещё идёт.
    busy: bool,
}

struct ScanResult {
    root: PathBuf,
    tree: DirNode,
    /// Сканирование остановили — данные неполные.
    cancelled: bool,
}

/// Что сделать после отрисовки.
enum Action {
    Open(PathBuf),
    Rescan,
    /// Открыть вложенную папку открытой папки.
    Enter(String),
    /// Подняться на столько уровней внутри результата.
    Up(usize),
    ExpandRest(Vec<usize>),
    CollapseRest,
    Back,
    Forward,
    /// Спросить, удалять ли элемент списка с этим номером; `true` — мимо корзины.
    AskDelete(usize, bool),
}

impl DiskTab {
    /// Открывает папку и запоминает переход для «Назад».
    pub fn open(&mut self, path: PathBuf) {
        self.record(|tab| tab.show(path));
    }

    /// «Назад»: сначала сворачивает раскрытое «Остальное», затем возвращает к предыдущей папке.
    pub fn go_back(&mut self) {
        if self.chart_stack.pop().is_some() {
            return;
        }
        let Some(path) = self.back.pop() else { return };
        if let Some(location) = self.location() {
            self.forward.push(location);
        }
        self.show(path);
    }

    /// «Вперёд»: к папке, из которой вернулись «Назад».
    pub fn go_forward(&mut self) {
        let Some(path) = self.forward.pop() else { return };
        if let Some(location) = self.location() {
            self.back.push(location);
        }
        self.show(path);
    }

    /// Выполняет переход `change` и, если папка сменилась, запоминает прежнюю для «Назад».
    fn record(&mut self, change: impl FnOnce(&mut Self)) {
        let before = self.location();
        change(self);
        if let Some(before) = before
            && self.location().as_ref() != Some(&before)
        {
            self.back.push(before);
            self.forward.clear();
        }
    }

    /// Папка, которая открыта или сканируется, чтобы открыться.
    fn location(&self) -> Option<PathBuf> {
        self.scan.as_ref().map(|scan| scan.root.clone()).or_else(|| self.current_path())
    }

    /// Открывает папку: если она внутри уже отсканированной — сразу, иначе сканирует её.
    fn show(&mut self, path: PathBuf) {
        self.error = None;
        if let Some(result) = &self.result
            && !result.cancelled
            && let Ok(relative) = path.strip_prefix(&result.root)
            && result.tree.find(relative).is_some()
        {
            self.path_input = path.display().to_string();
            self.current = relative.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
            self.refresh_entries();
            return;
        }
        self.start_scan(path);
    }

    fn start_scan(&mut self, path: PathBuf) {
        if let Some(scan) = &self.scan {
            scan.cancel();
        }
        self.path_input = path.display().to_string();
        if !path.is_dir() {
            self.error = Some(t!("disk.not_found", path = path.display()));
            return;
        }
        self.error = None;
        self.scan = Some(ScanJob::start(path));
    }

    /// Абсолютный путь открытой папки.
    fn current_path(&self) -> Option<PathBuf> {
        let result = self.result.as_ref()?;
        Some(self.current.iter().fold(result.root.clone(), |path, name| path.join(name)))
    }

    fn current_node(&self) -> Option<&DirNode> {
        let result = self.result.as_ref()?;
        result.tree.find(&self.current.iter().collect::<PathBuf>())
    }

    /// Перечитывает список открытой папки и сбрасывает раскрытие диаграммы.
    fn refresh_entries(&mut self) {
        self.chart_stack.clear();
        self.entries = match (self.current_node(), self.current_path()) {
            (Some(node), Some(path)) => disk_usage::entries(node, &path),
            _ => Vec::new(),
        };
        if let Some(path) = self.current_path() {
            self.path_input = path.display().to_string();
        }
    }

    fn poll_scan(&mut self, ui: &Ui) {
        if self.finish_scan() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Забирает результат законченного сканирования. Возвращает `true`, если сканирование ещё идёт.
    fn finish_scan(&mut self) -> bool {
        let Some(scan) = &self.scan else { return false };
        let Some(tree) = scan.try_result() else { return true };
        let root = scan.root.clone();
        let cancelled = scan.is_cancelled();
        self.scan = None;
        self.result = Some(ScanResult { root, tree, cancelled });
        self.current.clear();
        self.refresh_entries();
        false
    }

    pub fn ui(&mut self, ui: &mut Ui, window: Option<isize>, settings: &Settings, worker: &mut TrashWorker) {
        self.poll_scan(ui);
        if !self.drives_loaded {
            self.drives = drives::list();
            self.drives_loaded = true;
        }

        let mut action = None;
        self.toolbar(ui, window, &mut action);
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
        }

        if let Some(scan) = &self.scan {
            ui.horizontal(|ui| {
                ui.spinner();
                let files = scan.progress.files.load(Ordering::Relaxed);
                let size = format_size(scan.progress.bytes.load(Ordering::Relaxed));
                ui.label(t!("disk.scanning", path = scan.root.display(), files = files, size = size));
                if ui.button(t!("disk.stop")).clicked() {
                    scan.cancel();
                }
            });
        }

        if worker.owner() == Some(JobOwner::DiskTab) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(t!("disk.deleting"));
            });
        } else if let Some((message, is_error)) = &self.delete_message {
            let color = if *is_error { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
            ui.label(RichText::new(message).color(color));
        }

        if self.result.is_none() {
            if self.scan.is_none() {
                ui.add_space(12.0);
                ui.label(RichText::new(t!("disk.empty_hint")).weak());
            }
        } else {
            self.navigation(ui, &mut action);
            ui.separator();
            let context = DeleteContext {
                trash_enabled: settings.trash.is_enabled(),
                trash_dir: settings.trash.directory(),
                busy: worker.is_busy() || self.scan.is_some(),
            };
            self.content(ui, &mut action, &context);
        }

        if let Some(action) = action {
            match action {
                Action::AskDelete(index, permanent) => self.ask_delete(index, permanent, settings),
                action => self.apply(action),
            }
        }
        self.delete_dialog(ui.ctx(), settings, worker);
    }

    /// Готовит подтверждение удаления элемента списка.
    fn ask_delete(&mut self, index: usize, permanent: bool, settings: &Settings) {
        let (Some(entry), Some(dir)) = (self.entries.get(index), self.current_path()) else { return };
        let (is_dir, files) = match entry.kind {
            EntryKind::Dir { files, .. } => (true, files),
            EntryKind::File => (false, 1),
        };
        let permanent = permanent || !settings.trash.is_enabled();
        let trash_used = if permanent { 0 } else { Trash::new(settings.trash.directory()).used_bytes() };
        self.delete_request = Some(DeleteRequest {
            path: dir.join(&entry.name),
            parent: self.current.clone(),
            name: entry.name.clone(),
            is_dir,
            size: entry.size,
            files,
            permanent,
            trash_used,
            trash_limit: settings.trash.limit,
        });
    }

    /// Подтверждение удаления. Если в корзину не помещается — те же варианты, что при очистке.
    fn delete_dialog(&mut self, ctx: &egui::Context, settings: &Settings, worker: &mut TrashWorker) {
        let Some(request) = self.delete_request.clone() else { return };
        let fits = request.trash_limit.is_none_or(|limit| request.trash_used + request.size <= limit);
        let mut answer: Option<Option<(bool, TrashDecision)>> = None;

        let modal = Modal::new(Id::unique("disk_delete")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            let what = if request.is_dir {
                t!("disk.delete.folder", name = &request.name, size = format_size(request.size), files = request.files)
            } else {
                t!("disk.delete.file", name = &request.name, size = format_size(request.size))
            };
            if request.permanent {
                let title = RichText::new(t!("disk.delete.permanent_title")).heading();
                ui.add(Button::new((warning_atom(20.0), title)).frame(false));
                ui.label(t!("disk.delete.permanent_text", what = what));
            } else {
                ui.heading(t!("disk.delete.trash_title"));
                ui.label(t!("disk.delete.trash_text", what = what));
                if !fits {
                    let limit = request.trash_limit.map_or_else(String::new, format_size);
                    let text = t!("disk.delete.no_room", used = format_size(request.trash_used), limit = limit);
                    ui.label(RichText::new(text).color(ui.visuals().warn_fg_color));
                }
            }
            ui.add_space(8.0);

            let wide = |ui: &mut Ui, button: Button| ui.add(button.min_size(vec2(ui.available_width(), 0.0)));
            if request.permanent {
                let text = RichText::new(t!("disk.delete.confirm_permanent")).strong();
                if wide(ui, Button::new((warning_atom(16.0), text))).clicked() {
                    answer = Some(Some((true, TrashDecision::DeletePermanently)));
                }
            } else if fits {
                if wide(ui, Button::new(RichText::new(t!("disk.delete.confirm_trash")).strong())).clicked() {
                    answer = Some(Some((false, TrashDecision::IgnoreLimit)));
                }
            } else {
                let options = [
                    (TrashDecision::FreeSpace, t!("trash.limit.free_space"), t!("trash.limit.free_space.hint")),
                    (TrashDecision::IgnoreLimit, t!("trash.limit.ignore"), t!("trash.limit.ignore.hint")),
                ];
                for (decision, label, hint) in options {
                    if wide(ui, Button::new(label)).on_hover_text(hint).clicked() {
                        answer = Some(Some((false, decision)));
                    }
                }
                if wide(ui, Button::new((warning_atom(16.0), t!("trash.limit.delete")))).clicked() {
                    answer = Some(Some((true, TrashDecision::DeletePermanently)));
                }
            }
            if wide(ui, Button::new(t!("disk.delete.cancel"))).clicked() {
                answer = Some(None);
            }
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(None);
        }

        let Some(answer) = answer else { return };
        self.delete_request = None;
        let Some((permanent, decision)) = answer else { return };
        let trash = (!permanent).then(|| settings.trash.clone());
        worker.remove(JobOwner::DiskTab, request.path.clone(), t!("tabs.disk"), trash, decision);
        self.delete_message = None;
        self.deleting = Some(DeleteRequest { permanent, ..request });
    }

    /// Удаление закончилось: вычитаем удалённое из дерева и перечитываем открытую папку.
    pub fn removal_finished(&mut self, report: RemoveReport) {
        let Some(request) = self.deleting.take() else { return };
        if let Some(result) = &mut self.result {
            let whole = report.failed.is_empty();
            let mut path = request.parent.clone();
            if request.is_dir {
                path.push(request.name.clone());
            }
            result.tree.subtract(&path, report.bytes, report.files, request.is_dir && whole);
        }
        self.refresh_entries();

        let size = format_size(report.bytes);
        let mut message = if report.trash_run.is_some() {
            t!("disk.deleted_to_trash", files = report.files, size = size)
        } else {
            t!("disk.deleted_permanently", files = report.files, size = size)
        };
        if !report.failed.is_empty() {
            message += &t!("disk.delete_failed", count = report.failed.len());
            if let Some((path, reason)) = report.failed.first() {
                message += &format!("\n{}: {reason}", path.display());
            }
        }
        self.delete_message = Some((message, !report.failed.is_empty()));
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Open(path) => self.open(path),
            Action::Rescan => {
                if let Some(path) = self.current_path() {
                    self.start_scan(path);
                }
            }
            Action::Enter(name) => self.record(|tab| {
                tab.current.push(name);
                tab.refresh_entries();
            }),
            Action::Up(levels) => self.record(|tab| {
                tab.current.truncate(tab.current.len().saturating_sub(levels));
                tab.refresh_entries();
            }),
            Action::ExpandRest(items) => self.chart_stack.push(items),
            Action::CollapseRest => {
                self.chart_stack.pop();
            }
            Action::Back => self.go_back(),
            Action::Forward => self.go_forward(),
            // Удаление сначала подтверждается — см. `ask_delete`.
            Action::AskDelete(..) => {}
        }
    }

    /// Выбор диска, папки или пути.
    fn toolbar(&mut self, ui: &mut Ui, window: Option<isize>, action: &mut Option<Action>) {
        ui.horizontal(|ui| {
            ui.label(t!("disk.drive"));
            let combo = ComboBox::from_id_salt("disk_drive").selected_text(t!("disk.choose_drive")).width(220.0).show_ui(
                ui,
                |ui| {
                    for drive in &self.drives {
                        if ui.selectable_label(false, drive_label(drive)).clicked() {
                            *action = Some(Action::Open(drive.root.clone()));
                        }
                    }
                },
            );
            // Список дисков перечитывается при каждом открытии — так появляются подключённые флешки.
            if combo.response.clicked() {
                self.drives = drives::list();
            }

            if ui.button(t!("disk.pick_folder")).clicked() {
                let initial = self.current_path().or_else(|| Some(PathBuf::from(&self.path_input)));
                if let Some(path) = drives::pick_folder(window, initial.as_deref().filter(|path| path.is_dir())) {
                    *action = Some(Action::Open(path));
                }
            }
        });

        ui.horizontal(|ui| {
            let open_width = 90.0;
            let width = (ui.available_width() - open_width - ui.spacing().item_spacing.x).max(100.0);
            let edit = ui.add(TextEdit::singleline(&mut self.path_input).hint_text(t!("disk.path_hint")).desired_width(width));
            let enter = edit.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            let open = ui.add_sized([open_width, edit.rect.height()], Button::new(t!("disk.open"))).clicked();
            if (enter || open) && !self.path_input.trim().is_empty() {
                *action = Some(Action::Open(PathBuf::from(self.path_input.trim())));
            }
        });
    }

    /// «Вверх», путь по частям и действия с открытой папкой.
    fn navigation(&self, ui: &mut Ui, action: &mut Option<Action>) {
        let (Some(result), Some(path)) = (&self.result, self.current_path()) else { return };

        ui.horizontal_wrapped(|ui| {
            let can_go_back = !self.back.is_empty() || !self.chart_stack.is_empty();
            let back = ui.add_enabled(can_go_back, Button::new(t!("disk.back"))).on_hover_text(t!("disk.back.hint"));
            if back.clicked() {
                *action = Some(Action::Back);
            }
            let forward = ui.add_enabled(!self.forward.is_empty(), Button::new(t!("disk.forward")));
            if forward.on_hover_text(t!("disk.forward.hint")).clicked() {
                *action = Some(Action::Forward);
            }
            let parent = path.parent().map(Path::to_path_buf);
            if ui.add_enabled(parent.is_some(), Button::new(t!("disk.up"))).clicked() {
                *action = Some(if self.current.is_empty() {
                    // Выше корня сканирования — сканируем родительскую папку.
                    Action::Open(parent.unwrap_or_default())
                } else {
                    Action::Up(1)
                });
            }
            if ui.button(t!("disk.rescan")).on_hover_text(t!("disk.rescan.hint")).clicked() {
                *action = Some(Action::Rescan);
            }
            if ui.button(t!("disk.show_in_explorer")).clicked() {
                open_in_explorer(&path);
            }
            ui.separator();

            // Путь по частям: часть внутри результата открывается сразу, выше корня — сканируется.
            let parts: Vec<PathBuf> = path.ancestors().map(Path::to_path_buf).collect::<Vec<_>>().into_iter().rev().collect();
            let last = parts.len() - 1;
            for (depth, part) in parts.iter().enumerate() {
                let name = part
                    .file_name()
                    .map_or_else(|| part.display().to_string(), |name| name.to_string_lossy().into_owned());
                if depth == last {
                    ui.label(RichText::new(name).strong());
                } else {
                    if ui.link(name).clicked() {
                        *action = Some(if part.starts_with(&result.root) {
                            Action::Up(last - depth)
                        } else {
                            Action::Open(part.clone())
                        });
                    }
                    ui.label("›");
                }
            }
        });

        if let Some(node) = self.current_node() {
            let mut summary = t!("disk.summary", size = format_size(node.size), files = node.files);
            if self.current.is_empty()
                && let Some(drive) = self.drives.iter().find(|drive| drive.root == result.root)
            {
                summary += &t!("disk.free_space", free = format_size(drive.free), total = format_size(drive.total));
            }
            ui.label(RichText::new(summary).weak());
        }
        if result.cancelled {
            ui.label(RichText::new(t!("disk.cancelled")).color(ui.visuals().warn_fg_color));
        }
    }

    /// Список слева, диаграмма справа (если хватает ширины).
    fn content(&self, ui: &mut Ui, action: &mut Option<Action>, context: &DeleteContext) {
        let colors = self.slice_colors(&Palette::of(ui));
        let total = self.current_node().map_or(0, |node| node.size);
        let available = ui.available_size();
        let show_chart = available.x >= CHART_WIDTH * 2.2;
        let list_width = if show_chart { available.x - CHART_WIDTH - ui.spacing().item_spacing.x } else { available.x };

        // Колонкам задаём вертикальную раскладку явно: иначе они унаследуют горизонтальную
        // от строки, и строки списка лягут друг на друга.
        let column = Layout::top_down(Align::Min);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(vec2(list_width, available.y), column, |ui| {
                ui.set_width(list_width);
                self.list(ui, total, &colors, action, context);
            });
            if show_chart {
                ui.allocate_ui_with_layout(vec2(CHART_WIDTH, available.y), column, |ui| {
                    ui.set_width(CHART_WIDTH);
                    self.chart(ui, action);
                });
            }
        });
    }

    /// Содержимое открытой папки: от больших к меньшим.
    fn list(
        &self,
        ui: &mut Ui,
        total: u64,
        colors: &HashMap<usize, Color32>,
        action: &mut Option<Action>,
        context: &DeleteContext,
    ) {
        if self.entries.is_empty() {
            ui.label(RichText::new(t!("disk.empty_folder")).weak());
            return;
        }
        let Some(dir) = self.current_path() else { return };
        ScrollArea::vertical().auto_shrink(false).show_rows(ui, ROW_HEIGHT, self.entries.len(), |ui, rows| {
            for index in rows {
                let entry = &self.entries[index];
                let row = vec2(ui.available_width(), ROW_HEIGHT);
                ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
                    ui.set_height(ROW_HEIGHT);
                    let row = Row { index, entry, dir: &dir, total, color: colors.get(&index).copied() };
                    entry_row(ui, &row, action, context);
                });
            }
        });
    }

    /// Секторы на текущем уровне раскрытия: номера элементов списка и их размеры.
    fn chart_items(&self) -> Vec<usize> {
        match self.chart_stack.last() {
            Some(items) => items.clone(),
            None => (0..self.entries.len()).collect(),
        }
    }

    /// Цвет каждого элемента списка, попавшего на диаграмму отдельным сектором или в «Остальное».
    fn slice_colors(&self, palette: &Palette) -> HashMap<usize, Color32> {
        let items = self.chart_items();
        let sizes: Vec<u64> = items.iter().map(|&index| self.entries[index].size).collect();
        let mut colors = HashMap::new();
        for (number, slice) in disk_usage::slices(&sizes, MIN_SLICE_SHARE).into_iter().enumerate() {
            match slice {
                Slice::Item(item) => {
                    colors.insert(items[item], palette.chart(number));
                }
                Slice::Rest(rest) => {
                    for item in rest {
                        colors.insert(items[item], palette.chart_rest);
                    }
                }
            }
        }
        colors
    }

    fn chart(&self, ui: &mut Ui, action: &mut Option<Action>) {
        let items = self.chart_items();
        let sizes: Vec<u64> = items.iter().map(|&index| self.entries[index].size).collect();
        let total: u64 = sizes.iter().sum();
        let slices = disk_usage::slices(&sizes, MIN_SLICE_SHARE);

        if !self.chart_stack.is_empty() && ui.button(t!("disk.chart_back")).clicked() {
            *action = Some(Action::CollapseRest);
        }
        if slices.is_empty() {
            return;
        }

        let size = CHART_WIDTH - 10.0;
        let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
        let center = rect.center();
        let outer = size / 2.0 - 6.0;
        let inner = outer * 0.55;

        // Секторы по часовой стрелке от верхней точки.
        let palette = Palette::of(ui);
        let mut start = -FRAC_PI_2;
        let mut arcs = Vec::with_capacity(slices.len());
        for (number, slice) in slices.iter().enumerate() {
            let (size, color) = match slice {
                Slice::Item(item) => (sizes[*item], palette.chart(number)),
                Slice::Rest(rest) => (rest.iter().map(|&item| sizes[item]).sum(), palette.chart_rest),
            };
            let end = start + TAU * (size as f64 / total as f64) as f32;
            arcs.push((start, end, color, size));
            start = end;
        }

        let hovered = response.hover_pos().and_then(|pointer| slice_at(center, inner, outer, &arcs, pointer));
        let painter = ui.painter_at(rect);
        let background = ui.visuals().panel_fill;
        for (number, &(start, end, color, _)) in arcs.iter().enumerate() {
            // Сектор под указателем чуть выдвигается наружу.
            let grow = if hovered == Some(number) { 5.0 } else { 0.0 };
            paint_segment(&painter, center, inner, outer + grow, start, end, color);
            if arcs.len() > 1 {
                let direction = vec2(start.cos(), start.sin());
                painter.line_segment([center + direction * inner, center + direction * (outer + 6.0)], Stroke::new(1.5, background));
            }
        }

        // В центре — размер того, что на диаграмме.
        let text_color = ui.visuals().strong_text_color();
        painter.text(center, Align2::CENTER_CENTER, format_size(total), FontId::proportional(16.0), text_color);

        if let Some(number) = hovered {
            let (name, is_dir) = match &slices[number] {
                Slice::Item(item) => {
                    let entry = &self.entries[items[*item]];
                    (entry.name.clone(), matches!(entry.kind, EntryKind::Dir { kind: DirKind::Normal, .. }))
                }
                Slice::Rest(rest) => (t!("disk.rest", count = rest.len()), false),
            };
            let size = arcs[number].3;
            let share = share_text(size, total);
            response.clone().on_hover_text_at_pointer(format!("{name}\n{} · {share}", format_size(size)));
            if is_dir || matches!(slices[number], Slice::Rest(_)) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        if response.clicked()
            && let Some(number) = hovered
        {
            match &slices[number] {
                Slice::Item(item) => {
                    let entry = &self.entries[items[*item]];
                    if matches!(entry.kind, EntryKind::Dir { kind: DirKind::Normal, .. }) {
                        *action = Some(Action::Enter(entry.name.clone()));
                    }
                }
                Slice::Rest(rest) => *action = Some(Action::ExpandRest(rest.iter().map(|&item| items[item]).collect())),
            }
        }

        ui.add_space(6.0);
        ui.label(RichText::new(t!("disk.chart_hint", percent = (MIN_SLICE_SHARE * 100.0).round())).weak().small());
    }
}

/// Строка списка: элемент, его номер, папка, в которой он лежит, размер этой папки и цвет на диаграмме.
struct Row<'a> {
    index: usize,
    entry: &'a Entry,
    dir: &'a Path,
    total: u64,
    color: Option<Color32>,
}

/// Строка списка: цвет на диаграмме, имя, размер, доля, число файлов.
fn entry_row(ui: &mut Ui, row: &Row, action: &mut Option<Action>, context: &DeleteContext) {
    let Row { entry, dir, total, color, .. } = *row;
    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    if let Some(color) = color {
        ui.painter().circle_filled(dot.center(), 4.5, color);
    }

    let path = dir.join(&entry.name);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        // Отступ под полосу прокрутки.
        ui.add_space(ui.spacing().scroll.bar_width + 4.0);
        let files = match entry.kind {
            EntryKind::Dir { files, kind: DirKind::Normal } => t!("disk.files", count = files),
            EntryKind::Dir { kind: DirKind::Link, .. } => t!("disk.link"),
            EntryKind::Dir { kind: DirKind::Inaccessible, .. } => t!("disk.inaccessible"),
            EntryKind::File => String::new(),
        };
        ui.add_sized([110.0, ROW_HEIGHT], Label::new(RichText::new(files).weak()).truncate());
        share_bar(ui, entry.size, total, color);
        ui.add_sized([80.0, ROW_HEIGHT], Label::new(RichText::new(format_size(entry.size)).strong()));

        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            let is_dir = matches!(entry.kind, EntryKind::Dir { .. });
            let can_enter = matches!(entry.kind, EntryKind::Dir { kind: DirKind::Normal, .. });
            let name = if is_dir { format!("{}\\", entry.name) } else { entry.name.clone() };
            // Папку, которую можно открыть, видно по цвету ссылки; длинные имена обрезаются.
            let text = if can_enter { RichText::new(name).color(ui.visuals().hyperlink_color) } else { RichText::new(name) };
            let mut response = ui.add(Label::new(text).truncate().sense(Sense::click()));
            if can_enter {
                response = response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(t!("disk.enter_hint"));
            }
            if can_enter && response.clicked() {
                *action = Some(Action::Enter(entry.name.clone()));
            }
            entry_menu(&response, row, &path, action, context);
        });
    });
}

/// Меню по правому щелчку: открыть, показать в проводнике, скопировать путь, удалить.
fn entry_menu(response: &Response, row: &Row, path: &Path, action: &mut Option<Action>, context: &DeleteContext) {
    let is_dir = matches!(row.entry.kind, EntryKind::Dir { .. });
    let can_enter = matches!(row.entry.kind, EntryKind::Dir { kind: DirKind::Normal, .. });
    response.context_menu(|ui| {
        if can_enter && ui.button(t!("disk.menu.open")).clicked() {
            *action = Some(Action::Enter(row.entry.name.clone()));
        }
        if ui.button(t!("disk.menu.show_in_explorer")).clicked() {
            if is_dir {
                open_in_explorer(path);
            } else {
                select_in_explorer(path);
            }
        }
        if ui.button(t!("disk.menu.copy_path")).clicked() {
            ui.ctx().copy_text(path.display().to_string());
        }
        ui.separator();

        // С зажатым Shift пункт прямо в открытом меню меняется на «Удалить навсегда».
        let permanent = !context.trash_enabled || ui.input(|input| input.modifiers.shift);
        let protected = is_protected(path) || is_inside(path, &context.trash_dir) || is_inside(&context.trash_dir, path);
        let button = if permanent {
            Button::new((warning_atom(14.0), t!("disk.menu.delete_permanently")))
        } else {
            Button::new(t!("disk.menu.delete_to_trash"))
        };
        let hint = if protected {
            t!("disk.menu.delete_protected")
        } else if permanent {
            t!("disk.menu.delete_permanently.hint")
        } else {
            t!("disk.menu.delete_to_trash.hint")
        };
        let response = ui.add_enabled(!protected && !context.busy, button).on_hover_text(&hint).on_disabled_hover_text(hint);
        if response.clicked() {
            *action = Some(Action::AskDelete(row.index, permanent));
        }
    });
}

/// Лежит ли `path` внутри `dir` (или совпадает с ней). Без учёта регистра, как в Windows.
fn is_inside(path: &Path, dir: &Path) -> bool {
    let (path, dir) = (path.to_string_lossy().to_lowercase(), dir.to_string_lossy().to_lowercase());
    let dir = dir.trim_end_matches('\\');
    path.trim_end_matches('\\') == dir || path.starts_with(&format!("{dir}\\"))
}

/// Доля от открытой папки: полоска и проценты.
fn share_bar(ui: &mut Ui, size: u64, total: u64, color: Option<Color32>) {
    ui.add_sized([52.0, ROW_HEIGHT], Label::new(RichText::new(share_text(size, total)).weak()));
    let (rect, _) = ui.allocate_exact_size(vec2(70.0, 8.0), Sense::hover());
    let fraction = if total == 0 { 0.0 } else { size as f32 / total as f32 };
    let visuals = ui.visuals();
    ui.painter().rect_filled(rect, 2.0, visuals.extreme_bg_color);
    let filled = Rect::from_min_size(rect.min, vec2(rect.width() * fraction, rect.height()));
    ui.painter().rect_filled(filled, 2.0, color.unwrap_or(visuals.selection.bg_fill));
}

fn share_text(size: u64, total: u64) -> String {
    let percent = if total == 0 { 0.0 } else { size as f64 * 100.0 / total as f64 };
    format!("{percent:.1} %")
}

fn drive_label(drive: &Drive) -> String {
    let root = drive.root.display().to_string();
    let name = if drive.label.is_empty() { root } else { format!("{root} {}", drive.label) };
    t!("disk.drive_item", name = name, free = format_size(drive.free), total = format_size(drive.total))
}

/// Кольцевой сектор от угла `start` до `end` (радианы, по часовой стрелке).
fn paint_segment(painter: &egui::Painter, center: Pos2, inner: f32, outer: f32, start: f32, end: f32, color: Color32) {
    let steps = ((end - start) / 2f32.to_radians()).ceil().max(1.0) as u32;
    let mut mesh = Mesh::default();
    for step in 0..=steps {
        let angle = start + (end - start) * step as f32 / steps as f32;
        let direction = vec2(angle.cos(), angle.sin());
        mesh.colored_vertex(center + direction * outer, color);
        mesh.colored_vertex(center + direction * inner, color);
        if step > 0 {
            let base = step * 2;
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base + 1, base);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// Номер сектора под точкой `pointer`.
fn slice_at(center: Pos2, inner: f32, outer: f32, arcs: &[(f32, f32, Color32, u64)], pointer: Pos2) -> Option<usize> {
    let offset = pointer - center;
    if offset.length() < inner || offset.length() > outer + 6.0 {
        return None;
    }
    // Угол от верхней точки по часовой стрелке, как у секторов.
    let mut angle = offset.y.atan2(offset.x);
    if angle < -FRAC_PI_2 {
        angle += TAU;
    }
    arcs.iter().position(|&(start, end, _, _)| angle >= start && angle < end)
}

/// Открывает проводник с выделенным файлом.
fn select_in_explorer(path: &Path) {
    let _ = Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn wait_for_scan(tab: &mut DiskTab) {
        let started = std::time::Instant::now();
        while tab.finish_scan() {
            assert!(started.elapsed() < Duration::from_secs(10), "сканирование не закончилось");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn back_and_forward_walk_the_history() {
        let root = std::env::temp_dir().join(format!("hexcleaner-history-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/b")).unwrap();

        let mut tab = DiskTab::default();
        tab.open(root.clone());
        wait_for_scan(&mut tab);
        assert_eq!(tab.location(), Some(root.clone()));

        tab.apply(Action::Enter("a".into()));
        tab.apply(Action::Enter("b".into()));
        assert_eq!(tab.location(), Some(root.join("a").join("b")));

        tab.go_back();
        assert_eq!(tab.location(), Some(root.join("a")));
        tab.go_back();
        assert_eq!(tab.location(), Some(root.clone()));
        // Дальше назад некуда.
        tab.go_back();
        assert_eq!(tab.location(), Some(root.clone()));

        tab.go_forward();
        assert_eq!(tab.location(), Some(root.join("a")));
        // Новый переход стирает «Вперёд».
        tab.apply(Action::Up(1));
        assert_eq!(tab.location(), Some(root.clone()));
        tab.go_forward();
        assert_eq!(tab.location(), Some(root.clone()));

        // «Назад» сначала сворачивает раскрытое «Остальное».
        tab.apply(Action::ExpandRest(vec![0]));
        tab.go_back();
        assert!(tab.chart_stack.is_empty());
        assert_eq!(tab.location(), Some(root.clone()));
        tab.go_back();
        assert_eq!(tab.location(), Some(root.join("a")));
        let _ = fs::remove_dir_all(&root);
    }
}
