//! Вкладка «Настройки»: язык и тема, уведомления, журналы, корзина, пункт в меню проводника, обновления.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Checkbox, ComboBox, DragValue, Grid, RichText, ScrollArea, Slider, Ui};
use hexcleaner::catalog::CleanupTask;
use hexcleaner::i18n::{self, LanguageInfo};
use hexcleaner::settings::{Settings, Theme};
use hexcleaner::system::drives;
use hexcleaner::system::explorer_menu::{self, MenuState};
use hexcleaner::trash::{Compression, TrashPolicy, TrashSettings};
use hexcleaner::{t, update};

use crate::app::{open_in_explorer, trash_policy_name, window_title};
use crate::update_ui::Updater;

/// Деления ползунка размера корзины: 0 — корзина отключена, `None` — без ограничений.
const TRASH_LIMITS: [Option<u64>; 12] = [
    Some(0),
    Some(GB / 2),
    Some(GB),
    Some(2 * GB),
    Some(5 * GB),
    Some(10 * GB),
    Some(20 * GB),
    Some(50 * GB),
    Some(100 * GB),
    Some(200 * GB),
    Some(500 * GB),
    None,
];
const GB: u64 = 1024 * 1024 * 1024;

/// Самый большой лимит, который можно ввести числом, ГБ (больше — «без ограничений» на ползунке).
const MAX_LIMIT_GB: f64 = 100_000.0;

/// Срок хранения, который предлагается, когда его включают, и самый большой, дней.
const DEFAULT_KEEP_DAYS: u32 = 30;
const MAX_KEEP_DAYS: u32 = 3650;

/// Страница автора на Patreon.
const PATREON_PAGE: &str = "https://patreon.com/ModdingTechNik";

pub struct SettingsTab {
    /// Языки для списка выбора: встроенные и найденные в папке языков.
    languages: Vec<LanguageInfo>,
    /// Ошибка при смене языка или чтении файлов переводов.
    error: Option<String>,
    /// Путь к запущенной программе — её запускает пункт меню проводника.
    exe: Option<PathBuf>,
    /// Пункт в меню проводника по последнему чтению реестра.
    menu: MenuState,
    menu_error: Option<String>,
}

impl SettingsTab {
    pub fn new() -> Self {
        let mut tab = Self {
            languages: Vec::new(),
            error: None,
            exe: std::env::current_exe().ok(),
            menu: explorer_menu::state(),
            menu_error: None,
        };
        tab.refresh_languages();
        // Программу перенесли в другую папку — пункт меню ведёт в пустоту, переписываем его на эту копию.
        if tab.menu.installed && tab.menu.exe.as_deref().is_none_or(|exe| !exe.exists()) {
            tab.update_menu(true, tab.menu.shift_only, tab.exe.clone());
        }
        tab
    }

    /// `busy` — идёт очистка: язык не меняем, журнал пишется на одном языке.
    /// `window` — окно программы (для окна выбора папки корзины).
    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        settings: &mut Settings,
        tasks: &[CleanupTask],
        updater: &mut Updater,
        is_admin: bool,
        busy: bool,
        window: Option<isize>,
    ) {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            section(ui, t!("settings.appearance"));
            Grid::new("appearance").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(t!("settings.language"));
                ui.add_enabled_ui(!busy, |ui| self.language_selector(ui, settings, is_admin));
                ui.end_row();

                ui.label(t!("settings.theme"));
                theme_selector(ui, settings);
                ui.end_row();
            });
            if let Some(error) = &self.error {
                ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
            }

            section(ui, t!("settings.notifications"));
            let notify = ui.checkbox(&mut settings.notify_when_done, t!("settings.notify_when_done"));
            let flash = ui.checkbox(&mut settings.flash_taskbar, t!("settings.flash_taskbar"));
            if notify.on_hover_text(t!("settings.notify_when_done.hint")).changed() | flash.changed() {
                settings.save();
            }

            section(ui, t!("settings.logs"));
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    let compress = ui.checkbox(&mut settings.compress_old_logs, t!("settings.compress_logs"));
                    let mut changed = compress.on_hover_text(t!("settings.compress_logs.hint")).changed();
                    ui.add_enabled_ui(settings.compress_old_logs, |ui| {
                        let hints = [t!("settings.compress_logs.ntfs.hint"), t!("settings.compress_logs.zip.hint")];
                        changed |= compression_selector(ui, "log_compression", &mut settings.log_compression, hints);
                    });
                    if changed {
                        settings.save();
                    }
                });
            });

            section(ui, t!("settings.trash"));
            // Во время очистки корзину не перенастраиваем: очистка уже взяла настройки.
            ui.add_enabled_ui(!busy, |ui| {
                if trash_section(ui, &mut settings.trash, tasks, window) {
                    settings.save();
                }
            });

            section(ui, t!("settings.explorer"));
            self.explorer_section(ui);

            section(ui, t!("settings.updates"));
            updater.settings_section(ui, settings);
            ui.horizontal(|ui| {
                ui.hyperlink_to(t!("update.releases_page"), update::RELEASES_PAGE);
                ui.separator();
                ui.hyperlink_to(t!("settings.patreon"), PATREON_PAGE);
            });
        });
    }

    /// Пункт «Анализ папки в HexCleaner» в контекстном меню проводника.
    fn explorer_section(&mut self, ui: &mut Ui) {
        let mut installed = self.menu.installed;
        let mut shift_only = self.menu.shift_only;
        let menu = ui.checkbox(&mut installed, t!("settings.explorer_menu", label = t!("explorer_menu.label")));
        let menu_changed = menu.on_hover_text(t!("settings.explorer_menu.hint")).changed();
        let shift_changed = ui
            .indent("explorer_shift", |ui| {
                ui.add_enabled(installed, Checkbox::new(&mut shift_only, t!("settings.explorer_shift")))
                    .on_hover_text(t!("settings.explorer_shift.hint"))
            })
            .inner
            .changed();
        ui.label(RichText::new(t!("settings.explorer_menu.windows11")).weak());

        // Пункт запускает другую копию программы — например, старую или собранную из исходников.
        let mut rebind = false;
        if self.menu.installed
            && let (Some(registered), Some(exe)) = (&self.menu.exe, &self.exe)
            && !same_path(registered, exe)
        {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(t!("settings.explorer_other_copy", path = registered.display())).weak());
                rebind = ui.small_button(t!("settings.explorer_use_this_copy")).clicked();
            });
        }

        if menu_changed || shift_changed {
            // Включаем и меняем режим для той копии, на которую пункт уже указывает.
            let exe = self.menu.exe.clone().filter(|exe| exe.exists()).or_else(|| self.exe.clone());
            self.update_menu(installed, shift_only, exe);
        } else if rebind {
            self.update_menu(true, shift_only, self.exe.clone());
        }
        if let Some(error) = &self.menu_error {
            ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
        }
    }

    /// Добавляет, меняет или убирает пункт меню и перечитывает его из реестра.
    fn update_menu(&mut self, installed: bool, shift_only: bool, exe: Option<PathBuf>) {
        let result = match (installed, exe) {
            (true, Some(exe)) => explorer_menu::install(&exe, &t!("explorer_menu.label"), shift_only),
            (true, None) => Err(std::io::Error::other(t!("settings.explorer_no_exe"))),
            (false, _) => explorer_menu::uninstall(),
        };
        self.menu_error = result.err().map(|error| t!("settings.explorer_error", error = error));
        self.menu = explorer_menu::state();
    }

    /// Перечитывает список языков, чтобы в нём появились новые файлы из папки языков.
    fn refresh_languages(&mut self) {
        let (languages, broken_files) = i18n::available_languages();
        self.languages = languages;
        self.error = (!broken_files.is_empty())
            .then(|| t!("status.broken_language_files", files = broken_files.join(", ")));
    }

    fn language_selector(&mut self, ui: &mut Ui, settings: &mut Settings, is_admin: bool) {
        let current = settings.language.clone().unwrap_or_else(|| i18n::system_language().to_owned());
        let current_name = self
            .languages
            .iter()
            .find(|language| language.code == current)
            .map_or(current.clone(), |language| language.name.clone());

        let mut chosen = None;
        ui.horizontal(|ui| {
            let combo = ComboBox::from_id_salt("language").selected_text(current_name).width(180.0).show_ui(ui, |ui| {
                for language in &self.languages {
                    if ui.selectable_label(language.code == current, &language.name).clicked() {
                        chosen = Some(language.code.clone());
                    }
                }
            });
            // Список перечитывается при каждом открытии — так подхватываются новые файлы переводов.
            if combo.response.clicked() {
                self.refresh_languages();
            }
            combo.response.on_hover_text(t!("settings.language.hint"));

            let folder = ui.button(t!("settings.languages_folder")).on_hover_text(t!("settings.languages_folder.hint"));
            if folder.clicked() {
                i18n::export_built_in_languages();
                open_in_explorer(&i18n::languages_directory());
            }
        });

        let Some(code) = chosen else { return };
        match i18n::set_language(&code) {
            Ok(()) => {
                settings.language = Some(code);
                settings.save();
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(window_title(is_admin)));
                // Текст пункта в меню проводника — на новом языке.
                if self.menu.installed {
                    let exe = self.menu.exe.clone().or_else(|| self.exe.clone());
                    self.update_menu(true, self.menu.shift_only, exe);
                }
            }
            Err(error) => self.error = Some(t!("status.language_failed", code = code, error = error)),
        }
    }
}

fn theme_selector(ui: &mut Ui, settings: &mut Settings) {
    let mut theme = settings.theme;
    ComboBox::from_id_salt("theme")
        .selected_text(theme_name(theme))
        .width(180.0)
        .show_ui(ui, |ui| {
            for option in [Theme::Dark, Theme::Light, Theme::System] {
                ui.selectable_value(&mut theme, option, theme_name(option));
            }
        })
        .response
        .on_hover_text(t!("settings.theme.hint"));

    if theme != settings.theme {
        settings.theme = theme;
        settings.save();
        ui.ctx().set_theme(egui_theme(theme));
    }
}

/// Пути Windows сравниваются без учёта регистра.
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// Заголовок раздела с отступом сверху.
fn section(ui: &mut Ui, title: String) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong().size(16.0));
    ui.add_space(2.0);
}

fn theme_name(theme: Theme) -> String {
    match theme {
        Theme::Dark => t!("theme.dark"),
        Theme::Light => t!("theme.light"),
        Theme::System => t!("theme.system"),
    }
}

pub fn egui_theme(theme: Theme) -> egui::ThemePreference {
    match theme {
        Theme::Dark => egui::ThemePreference::Dark,
        Theme::Light => egui::ThemePreference::Light,
        Theme::System => egui::ThemePreference::System,
    }
}

/// Раздел «Корзина». Возвращает `true`, если что-то изменилось.
fn trash_section(ui: &mut Ui, trash: &mut TrashSettings, tasks: &[CleanupTask], window: Option<isize>) -> bool {
    let mut changed = false;

    // Размер: ползунок по делениям, от «отключена» до «без ограничений».
    let mut step = TRASH_LIMITS
        .iter()
        .position(|&limit| limit == trash.limit)
        .unwrap_or_else(|| nearest_limit_step(trash.limit));
    ui.horizontal(|ui| {
        ui.label(t!("settings.trash.limit"));
        let slider = ui.add(Slider::new(&mut step, 0..=TRASH_LIMITS.len() - 1).show_value(false));
        if slider.on_hover_text(t!("settings.trash.limit.hint")).changed() {
            trash.limit = TRASH_LIMITS[step];
            changed = true;
        }
        // Рядом — точное число гигабайт: его можно ввести с клавиатуры.
        match trash.limit {
            Some(limit) => {
                let mut gigabytes = limit as f64 / GB as f64;
                let field = DragValue::new(&mut gigabytes)
                    .range(0.0..=MAX_LIMIT_GB)
                    .speed(0.1)
                    .max_decimals(1)
                    .suffix(format!(" {}", t!("unit.gigabytes")));
                if ui.add(field).on_hover_text(t!("settings.trash.limit.field_hint")).changed() {
                    trash.limit = Some((gigabytes * GB as f64).round() as u64);
                    changed = true;
                }
                if limit == 0 {
                    ui.label(RichText::new(t!("trash.limit_disabled")).weak());
                }
            }
            None => {
                ui.label(RichText::new(t!("trash.limit_unlimited")).strong());
            }
        }
    });

    ui.add_enabled_ui(trash.is_enabled(), |ui| {
        // Срок хранения: очистки старше удаляются при запуске программы и перед очисткой.
        ui.horizontal(|ui| {
            let mut limited = trash.keep_days.is_some();
            let checkbox = ui.checkbox(&mut limited, t!("settings.trash.keep"));
            if checkbox.on_hover_text(t!("settings.trash.keep.hint")).changed() {
                trash.keep_days = limited.then_some(DEFAULT_KEEP_DAYS);
                changed = true;
            }
            if let Some(days) = &mut trash.keep_days {
                let field = DragValue::new(days).range(1..=MAX_KEEP_DAYS).suffix(format!(" {}", t!("settings.trash.days")));
                changed |= ui.add(field).changed();
            }
        });

        ui.horizontal(|ui| {
            let compress = ui.checkbox(&mut trash.compress, t!("settings.trash.compress"));
            changed |= compress.on_hover_text(t!("settings.trash.compress.hint")).changed();
            ui.add_enabled_ui(trash.compress, |ui| {
                let hints = [t!("settings.trash.ntfs.hint"), t!("settings.trash.zip.hint")];
                changed |= compression_selector(ui, "trash_compression", &mut trash.compression, hints);
            });
        });

        // Папка корзины.
        let dir = trash.directory();
        ui.horizontal_wrapped(|ui| {
            ui.label(t!("settings.trash.folder"));
            ui.label(RichText::new(dir.display().to_string()).weak());
        });
        ui.horizontal(|ui| {
            if ui.button(t!("settings.trash.change_folder")).on_hover_text(t!("settings.trash.change_folder.hint")).clicked()
                && let Some(path) = drives::pick_folder(window, Some(&dir).filter(|dir| dir.is_dir()).map(|dir| dir.as_path()))
            {
                trash.path = Some(path);
                changed = true;
            }
            if trash.path.is_some() && ui.button(t!("settings.trash.default_folder")).clicked() {
                trash.path = None;
                changed = true;
            }
            if ui.button(t!("settings.trash.open_folder")).clicked() {
                let _ = std::fs::create_dir_all(&dir);
                open_in_explorer(&dir);
            }
        });

        // Что делать с файлами каждого пункта.
        ui.collapsing(t!("settings.trash.policies"), |ui| {
            ui.label(RichText::new(t!("settings.trash.policies.hint")).weak());
            Grid::new("trash_policies").num_columns(2).spacing([12.0, 4.0]).striped(true).show(ui, |ui| {
                for task in tasks.iter().filter(|task| task.deletes_files()) {
                    ui.label(task.title());
                    let mut policy = trash.policy(task.id);
                    ComboBox::from_id_salt(("trash_policy", task.id))
                        .selected_text(trash_policy_name(policy))
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for option in [TrashPolicy::Normal, TrashPolicy::Always, TrashPolicy::Never] {
                                ui.selectable_value(&mut policy, option, trash_policy_name(option));
                            }
                        });
                    if policy != trash.policy(task.id) {
                        trash.set_policy(task.id, policy);
                        changed = true;
                    }
                    ui.end_row();
                }
            });
        });
    });
    changed
}

/// Выбор способа сжатия: NTFS или ZIP. `hints` — подсказки к ним в том же порядке.
/// Возвращает `true`, если выбор изменился.
fn compression_selector(ui: &mut Ui, id: &str, compression: &mut Compression, hints: [String; 2]) -> bool {
    let name = |compression| match compression {
        Compression::Ntfs => t!("settings.compression.ntfs"),
        Compression::Zip => t!("settings.compression.zip"),
    };
    let before = *compression;
    ComboBox::from_id_salt(id).selected_text(name(*compression)).show_ui(ui, |ui| {
        for (option, hint) in [Compression::Ntfs, Compression::Zip].into_iter().zip(hints) {
            ui.selectable_value(compression, option, name(option)).on_hover_text(hint);
        }
    });
    *compression != before
}

/// Ближайшее деление ползунка к лимиту, которого нет среди делений (например, вписанного в настройки вручную).
fn nearest_limit_step(limit: Option<u64>) -> usize {
    let Some(limit) = limit else { return TRASH_LIMITS.len() - 1 };
    TRASH_LIMITS
        .iter()
        .enumerate()
        .filter_map(|(step, value)| value.map(|value| (step, value.abs_diff(limit))))
        .min_by_key(|&(_, distance)| distance)
        .map_or(0, |(step, _)| step)
}
