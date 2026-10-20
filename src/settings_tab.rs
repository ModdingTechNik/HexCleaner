//! Вкладка «Настройки»: язык и тема, уведомления, журналы, обновления.

use eframe::egui::{self, ComboBox, Grid, RichText, ScrollArea, Ui};
use hexcleaner::i18n::{self, LanguageInfo};
use hexcleaner::settings::{Settings, Theme};
use hexcleaner::{t, update};

use crate::app::{open_in_explorer, window_title};
use crate::update_ui::Updater;

/// Страница автора на Patreon.
const PATREON_PAGE: &str = "https://patreon.com/ModdingTechNik";

pub struct SettingsTab {
    /// Языки для списка выбора: встроенные и найденные в папке языков.
    languages: Vec<LanguageInfo>,
    /// Ошибка при смене языка или чтении файлов переводов.
    error: Option<String>,
}

impl SettingsTab {
    pub fn new() -> Self {
        let mut tab = Self { languages: Vec::new(), error: None };
        tab.refresh_languages();
        tab
    }

    /// `busy` — идёт очистка: язык не меняем, журнал пишется на одном языке.
    pub fn ui(&mut self, ui: &mut Ui, settings: &mut Settings, updater: &mut Updater, is_admin: bool, busy: bool) {
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
            let compress = ui.add_enabled(
                !busy,
                egui::Checkbox::new(&mut settings.compress_old_logs, t!("settings.compress_logs")),
            );
            if compress.on_hover_text(t!("settings.compress_logs.hint")).changed() {
                settings.save();
            }

            section(ui, t!("settings.updates"));
            updater.settings_section(ui, settings);
            ui.horizontal(|ui| {
                ui.hyperlink_to(t!("update.releases_page"), update::RELEASES_PAGE);
                ui.separator();
                ui.hyperlink_to(t!("settings.patreon"), PATREON_PAGE);
            });
        });
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
