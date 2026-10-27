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
    self, Align, Align2, Button, Color32, ComboBox, FontId, Key, Label, Layout, Mesh, Pos2, Rect, Response,
    RichText, ScrollArea, Sense, Shape, Stroke, TextEdit, Ui, vec2,
};
use hexcleaner::cleaner::format_size;
use hexcleaner::disk_usage::{self, DirKind, DirNode, Entry, EntryKind, ScanJob, Slice};
use hexcleaner::system::drives::{self, Drive};
use hexcleaner::t;

use crate::app::open_in_explorer;

/// Элементы меньше этой доли объединяются в сектор «Остальное».
const MIN_SLICE_SHARE: f64 = 0.03;

/// Ширина диаграммы справа от списка.
const CHART_WIDTH: f32 = 250.0;

/// Высота строки списка.
const ROW_HEIGHT: f32 = 22.0;

/// Цвета секторов. Подобраны так, чтобы читаться и на тёмной, и на светлой теме.
const PALETTE: [Color32; 10] = [
    Color32::from_rgb(0x4E, 0x79, 0xA7),
    Color32::from_rgb(0xF2, 0x8E, 0x2B),
    Color32::from_rgb(0xE1, 0x57, 0x59),
    Color32::from_rgb(0x76, 0xB7, 0xB2),
    Color32::from_rgb(0x59, 0xA1, 0x4F),
    Color32::from_rgb(0xED, 0xC9, 0x48),
    Color32::from_rgb(0xB0, 0x7A, 0xA1),
    Color32::from_rgb(0xFF, 0x9D, 0xA7),
    Color32::from_rgb(0x9C, 0x75, 0x5F),
    Color32::from_rgb(0x5F, 0xA2, 0xCE),
];

/// Цвет сектора «Остальное».
const REST_COLOR: Color32 = Color32::from_rgb(0x9A, 0x9A, 0x9A);

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
    error: Option<String>,
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
}

impl DiskTab {
    /// Открывает папку: если она внутри уже отсканированной — сразу, иначе сканирует её.
    pub fn open(&mut self, path: PathBuf) {
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
        let Some(scan) = &self.scan else { return };
        match scan.try_result() {
            Some(tree) => {
                let root = scan.root.clone();
                let cancelled = scan.is_cancelled();
                self.scan = None;
                self.result = Some(ScanResult { root, tree, cancelled });
                self.current.clear();
                self.refresh_entries();
            }
            None => ui.ctx().request_repaint_after(Duration::from_millis(100)),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, window: Option<isize>) {
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

        if self.result.is_none() {
            if self.scan.is_none() {
                ui.add_space(12.0);
                ui.label(RichText::new(t!("disk.empty_hint")).weak());
            }
        } else {
            self.navigation(ui, &mut action);
            ui.separator();
            self.content(ui, &mut action);
        }

        if let Some(action) = action {
            self.apply(action);
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Open(path) => self.open(path),
            Action::Rescan => {
                if let Some(path) = self.current_path() {
                    self.start_scan(path);
                }
            }
            Action::Enter(name) => {
                self.current.push(name);
                self.refresh_entries();
            }
            Action::Up(levels) => {
                self.current.truncate(self.current.len().saturating_sub(levels));
                self.refresh_entries();
            }
            Action::ExpandRest(items) => self.chart_stack.push(items),
            Action::CollapseRest => {
                self.chart_stack.pop();
            }
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
    fn content(&self, ui: &mut Ui, action: &mut Option<Action>) {
        let colors = self.slice_colors();
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
                self.list(ui, total, &colors, action);
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
    fn list(&self, ui: &mut Ui, total: u64, colors: &HashMap<usize, Color32>, action: &mut Option<Action>) {
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
                    entry_row(ui, entry, &dir, total, colors.get(&index).copied(), action);
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
    fn slice_colors(&self) -> HashMap<usize, Color32> {
        let items = self.chart_items();
        let sizes: Vec<u64> = items.iter().map(|&index| self.entries[index].size).collect();
        let mut colors = HashMap::new();
        for (number, slice) in disk_usage::slices(&sizes, MIN_SLICE_SHARE).into_iter().enumerate() {
            match slice {
                Slice::Item(item) => {
                    colors.insert(items[item], PALETTE[number % PALETTE.len()]);
                }
                Slice::Rest(rest) => {
                    for item in rest {
                        colors.insert(items[item], REST_COLOR);
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
        let mut start = -FRAC_PI_2;
        let mut arcs = Vec::with_capacity(slices.len());
        for (number, slice) in slices.iter().enumerate() {
            let (size, color) = match slice {
                Slice::Item(item) => (sizes[*item], PALETTE[number % PALETTE.len()]),
                Slice::Rest(rest) => (rest.iter().map(|&item| sizes[item]).sum(), REST_COLOR),
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

/// Строка списка: цвет на диаграмме, имя, размер, доля, число файлов.
fn entry_row(ui: &mut Ui, entry: &Entry, dir: &Path, total: u64, color: Option<Color32>, action: &mut Option<Action>) {
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
            entry_menu(&response, &path, is_dir, can_enter, &entry.name, action);
        });
    });
}

/// Меню по правому щелчку: открыть, показать в проводнике, скопировать путь.
fn entry_menu(response: &Response, path: &Path, is_dir: bool, can_enter: bool, name: &str, action: &mut Option<Action>) {
    response.context_menu(|ui| {
        if can_enter && ui.button(t!("disk.menu.open")).clicked() {
            *action = Some(Action::Enter(name.to_owned()));
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
    });
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
