//! Тема оформления окна: цвета из файла темы (см. `hexcleaner::themes`) поверх тёмной
//! или светлой темы egui и цвета самой программы ([`Palette`]).
//!
//! Названия цветов в файле темы — [`SLOTS`] (интерфейс egui) и [`Palette::set`] (программа).
//! Встроенные темы задают их все теми же значениями, что у egui и прежней программы,
//! поэтому служат и образцом: какие цвета бывают и как они выглядят сейчас.

use eframe::egui::{self, Color32, Context, Id, Ui, Visuals};
use hexcleaner::catalog::Safety;
use hexcleaner::t;
use hexcleaner::themes::{self, Base, ThemeFile};

/// Цвет интерфейса egui, который можно задать в теме.
struct Slot {
    key: &'static str,
    /// Нужен тестам: встроенные темы сверяются с цветами egui.
    #[cfg_attr(not(test), allow(dead_code))]
    get: fn(&Visuals) -> Color32,
    set: fn(&mut Visuals, Color32),
}

/// Цвета интерфейса: название в файле темы и поле egui.
const SLOTS: &[Slot] = &[
    // Фон окна и панелей.
    Slot { key: "background", get: |v| v.panel_fill, set: |v, c| v.panel_fill = c },
    // Диалоги, выпадающие списки, подсказки.
    Slot { key: "window", get: |v| v.window_fill, set: |v, c| v.window_fill = c },
    Slot { key: "window_border", get: |v| v.window_stroke.color, set: |v, c| v.window_stroke.color = c },
    // Поля ввода, полосы прогресса, фон прокрутки.
    Slot { key: "input", get: |v| v.extreme_bg_color, set: |v, c| v.extreme_bg_color = c },
    // Полосы через строку в таблицах.
    Slot { key: "stripe", get: |v| v.faint_bg_color, set: |v, c| v.faint_bg_color = c },
    Slot {
        key: "text",
        get: |v| v.widgets.noninteractive.fg_stroke.color,
        set: |v, c| v.widgets.noninteractive.fg_stroke.color = c,
    },
    // Пояснения и второстепенный текст.
    Slot { key: "text_weak", get: |v| v.weak_text_color(), set: |v, c| v.weak_text_color = Some(c) },
    // Разделители и рамки групп.
    Slot {
        key: "border",
        get: |v| v.widgets.noninteractive.bg_stroke.color,
        set: |v, c| v.widgets.noninteractive.bg_stroke.color = c,
    },
    // Кнопки, флажки, ползунки — в покое, под мышью, нажатые и раскрытые (списки).
    Slot { key: "button", get: |v| v.widgets.inactive.weak_bg_fill, set: |v, c| set_fill(&mut v.widgets.inactive, c) },
    Slot {
        key: "button_text",
        get: |v| v.widgets.inactive.fg_stroke.color,
        set: |v, c| v.widgets.inactive.fg_stroke.color = c,
    },
    Slot {
        key: "button_hovered",
        get: |v| v.widgets.hovered.weak_bg_fill,
        set: |v, c| set_fill(&mut v.widgets.hovered, c),
    },
    Slot {
        key: "button_hovered_text",
        get: |v| v.widgets.hovered.fg_stroke.color,
        set: |v, c| v.widgets.hovered.fg_stroke.color = c,
    },
    Slot {
        key: "button_hovered_border",
        get: |v| v.widgets.hovered.bg_stroke.color,
        set: |v, c| v.widgets.hovered.bg_stroke.color = c,
    },
    Slot { key: "button_pressed", get: |v| v.widgets.active.weak_bg_fill, set: |v, c| set_fill(&mut v.widgets.active, c) },
    Slot {
        key: "button_pressed_text",
        get: |v| v.widgets.active.fg_stroke.color,
        set: |v, c| v.widgets.active.fg_stroke.color = c,
    },
    Slot { key: "button_open", get: |v| v.widgets.open.weak_bg_fill, set: |v, c| set_fill(&mut v.widgets.open, c) },
    // Выделение: выбранный пункт списка, заполнение ползунка, выделенный текст.
    Slot { key: "accent", get: |v| v.selection.bg_fill, set: |v, c| v.selection.bg_fill = c },
    Slot { key: "accent_text", get: |v| v.selection.stroke.color, set: |v, c| v.selection.stroke.color = c },
    Slot { key: "link", get: |v| v.hyperlink_color, set: |v, c| v.hyperlink_color = c },
    Slot { key: "warning", get: |v| v.warn_fg_color, set: |v, c| v.warn_fg_color = c },
    Slot { key: "error", get: |v| v.error_fg_color, set: |v, c| v.error_fg_color = c },
];

/// Фон кнопки: и у самой кнопки (`weak_bg_fill`), и у флажков и ползунков (`bg_fill`).
fn set_fill(widget: &mut egui::style::WidgetVisuals, color: Color32) {
    widget.weak_bg_fill = color;
    widget.bg_fill = color;
}

/// Цвета самой программы.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// Итоги очистки, безопасные пункты.
    pub success: Color32,
    /// Итоги анализа, пункты средней безопасности.
    pub found: Color32,
    /// Ошибки, рискованные пункты, кнопка «Удалить навсегда».
    pub failure: Color32,
    /// Секторы на вкладке «Место на диске», по кругу.
    pub chart: Vec<Color32>,
    /// Сектор «Остальное».
    pub chart_rest: Color32,
}

impl Palette {
    /// Цвета прежних встроенных тем — на случай, если в теме их нет.
    fn default_for(base: Base) -> Self {
        let chart = [
            [0x4E, 0x79, 0xA7],
            [0xF2, 0x8E, 0x2B],
            [0xE1, 0x57, 0x59],
            [0x76, 0xB7, 0xB2],
            [0x59, 0xA1, 0x4F],
            [0xED, 0xC9, 0x48],
            [0xB0, 0x7A, 0xA1],
            [0xFF, 0x9D, 0xA7],
            [0x9C, 0x75, 0x5F],
            [0x5F, 0xA2, 0xCE],
        ]
        .map(|[r, g, b]| Color32::from_rgb(r, g, b))
        .to_vec();
        let chart_rest = Color32::from_rgb(0x9A, 0x9A, 0x9A);
        match base {
            Base::Dark => Self {
                success: Color32::from_rgb(0x7C, 0xC4, 0x7F),
                found: Color32::from_rgb(0xE8, 0xB9, 0x4A),
                failure: Color32::from_rgb(0xE5, 0x6B, 0x6B),
                chart,
                chart_rest,
            },
            Base::Light => Self {
                success: Color32::from_rgb(0x2E, 0x7D, 0x32),
                found: Color32::from_rgb(0xA8, 0x5F, 0x00),
                failure: Color32::from_rgb(0xC6, 0x28, 0x28),
                chart,
                chart_rest,
            },
        }
    }

    /// Цвета для текущей темы окна.
    pub fn of(ui: &Ui) -> Self {
        let ctx = ui.ctx();
        ctx.data(|data| data.get_temp(palette_id(ctx.theme()))).unwrap_or_else(|| {
            Self::default_for(if ui.visuals().dark_mode { Base::Dark } else { Base::Light })
        })
    }

    /// Задаёт цвет программы по названию из файла темы; `false` — такого цвета нет.
    fn set(&mut self, key: &str, color: Color32) -> bool {
        match key {
            "success" => self.success = color,
            "found" => self.found = color,
            "failure" => self.failure = color,
            "chart_rest" => self.chart_rest = color,
            _ => return false,
        }
        true
    }

    /// Цвет точки уровня безопасности: зелёный, жёлтый, красный.
    pub fn safety(&self, safety: Safety) -> Color32 {
        match safety {
            Safety::Safe => self.success,
            Safety::Moderate => self.found,
            Safety::Caution => self.failure,
        }
    }

    /// Цвет `number`-го сектора диаграммы.
    pub fn chart(&self, number: usize) -> Color32 {
        self.chart.get(number % self.chart.len().max(1)).copied().unwrap_or(self.chart_rest)
    }
}

/// Где в памяти egui лежит палитра темы `theme` (у тёмной и светлой — своя).
fn palette_id(theme: egui::Theme) -> Id {
    Id::unique("hexcleaner_palette").with(theme)
}

/// Включает тему `id` или, для [`themes::SYSTEM`], встроенные тёмную и светлую по настройке Windows.
/// Возвращает, что пошло не так: тема не загрузилась (тогда включается тема по умолчанию)
/// или в ней есть цвета, которые программа не поняла.
pub fn apply(ctx: &Context, id: &str) -> Option<String> {
    let mut problems = Vec::new();
    let preference = if id == themes::SYSTEM {
        for built_in in ["dark", "light"] {
            install(ctx, built_in, &mut problems);
        }
        egui::ThemePreference::System
    } else {
        match install(ctx, id, &mut problems) {
            Some(theme) => theme.into(),
            None => install(ctx, themes::DEFAULT, &mut problems).unwrap_or(egui::Theme::Dark).into(),
        }
    };
    ctx.set_theme(preference);
    (!problems.is_empty()).then(|| problems.join(" "))
}

/// Загружает тему и ставит её цвета для её основы (тёмной или светлой темы egui).
fn install(ctx: &Context, id: &str, problems: &mut Vec<String>) -> Option<egui::Theme> {
    let file = match themes::load(id) {
        Ok(file) => file,
        Err(error) => {
            problems.push(t!("status.theme_failed", id = id, error = error));
            return None;
        }
    };
    let (visuals, palette, unknown) = build(&file);
    if !unknown.is_empty() {
        problems.push(t!("status.theme_unknown_colors", id = id, colors = unknown.join(", ")));
    }
    let theme = match file.base() {
        Base::Dark => egui::Theme::Dark,
        Base::Light => egui::Theme::Light,
    };
    ctx.set_visuals_of(theme, visuals);
    ctx.data_mut(|data| data.insert_temp(palette_id(theme), palette));
    Some(theme)
}

/// Цвета egui и программы по файлу темы и названия цветов, которые не удалось применить.
fn build(file: &ThemeFile) -> (Visuals, Palette, Vec<String>) {
    let mut visuals = match file.base() {
        Base::Dark => Visuals::dark(),
        Base::Light => Visuals::light(),
    };
    let mut palette = Palette::default_for(file.base());
    let mut unknown = Vec::new();

    for (key, value) in &file.colors {
        let Ok(color) = Color32::from_hex(value.trim()) else {
            unknown.push(format!("{key} = {value}"));
            continue;
        };
        match SLOTS.iter().find(|slot| slot.key == key) {
            Some(slot) => (slot.set)(&mut visuals, color),
            None if palette.set(key, color) => {}
            None => unknown.push(key.clone()),
        }
    }
    if !file.chart.is_empty() {
        let mut chart = Vec::with_capacity(file.chart.len());
        for (index, value) in file.chart.iter().enumerate() {
            match Color32::from_hex(value.trim()) {
                Ok(color) => chart.push(color),
                Err(_) => unknown.push(format!("chart[{index}] = {value}")),
            }
        }
        if !chart.is_empty() {
            palette.chart = chart;
        }
    }
    (visuals, palette, unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built_in(id: &str) -> ThemeFile {
        // Только встроенная тема — без файлов из папки тем этого компьютера.
        let json = match id {
            "dark" => include_str!("../themes/dark.json"),
            _ => include_str!("../themes/light.json"),
        };
        serde_json::from_str(json).unwrap()
    }

    /// `#RRGGBB`, а с прозрачностью — `#RRGGBBAA`.
    fn hex(color: Color32) -> String {
        let [r, g, b, a] = color.to_srgba_unmultiplied();
        if a == 255 { format!("#{r:02X}{g:02X}{b:02X}") } else { format!("#{r:02X}{g:02X}{b:02X}{a:02X}") }
    }

    /// Встроенные темы задают все цвета — и ровно такими, как у egui и прежней программы:
    /// с ними окно выглядит так же, как до тем в файлах.
    #[test]
    fn built_in_themes_match_egui_and_old_palette() {
        for (id, base, defaults) in [("dark", Base::Dark, Visuals::dark()), ("light", Base::Light, Visuals::light())] {
            let file = built_in(id);
            let (visuals, palette, unknown) = build(&file);
            assert!(unknown.is_empty(), "{id}: непонятные цвета {unknown:?}");
            for slot in SLOTS {
                assert!(file.colors.contains_key(slot.key), "{id}: нет цвета {}", slot.key);
                let expected = match slot.key {
                    // У egui полосы прибавляют яркость к фону (цвет с нулевой прозрачностью) —
                    // в #RRGGBBAA так не записать. Во встроенной теме — готовый цвет полосы поверх фона.
                    "stripe" => defaults.panel_fill + defaults.faint_bg_color,
                    _ => (slot.get)(&defaults),
                };
                assert_eq!(hex((slot.get)(&visuals)), hex(expected), "{id}: {}", slot.key);
            }
            assert_eq!(palette, Palette::default_for(base), "{id}: цвета программы");
            for key in ["success", "found", "failure", "chart_rest"] {
                assert!(file.colors.contains_key(key), "{id}: нет цвета {key}");
            }
            assert!(!file.chart.is_empty(), "{id}: нет цветов диаграммы");
        }
    }

    #[test]
    fn applies_colors_and_reports_unknown_ones() {
        let file: ThemeFile = serde_json::from_str(
            r##"{"base": "light", "colors": {"accent": "#0077BE", "found": "#123", "glow": "#FFFFFF", "text": "red"},
                 "chart": ["#000000", "oops"]}"##,
        )
        .unwrap();
        let (visuals, palette, unknown) = build(&file);
        assert!(!visuals.dark_mode);
        assert_eq!(visuals.selection.bg_fill, Color32::from_rgb(0x00, 0x77, 0xBE));
        assert_eq!(palette.found, Color32::from_rgb(0x11, 0x22, 0x33));
        assert_eq!(palette.chart, [Color32::BLACK]);
        assert_eq!(unknown, ["glow", "text = red", "chart[1] = oops"]);
    }

    /// Печатает цвета egui и программы в виде JSON — для встроенных тем после обновления egui:
    /// `cargo test --bin HexCleaner print_built_in_colors -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_built_in_colors() {
        for (id, base, visuals) in [("dark", Base::Dark, Visuals::dark()), ("light", Base::Light, Visuals::light())] {
            let palette = Palette::default_for(base);
            let mut colors: Vec<(&str, Color32)> = SLOTS.iter().map(|slot| (slot.key, (slot.get)(&visuals))).collect();
            colors.extend([
                ("success", palette.success),
                ("found", palette.found),
                ("failure", palette.failure),
                ("chart_rest", palette.chart_rest),
            ]);
            println!("== {id}");
            for (key, color) in colors {
                println!("    \"{key}\": \"{}\",", hex(color));
            }
            let chart: Vec<String> = palette.chart.iter().map(|&color| format!("\"{}\"", hex(color))).collect();
            println!("  \"chart\": [{}]", chart.join(", "));
        }
    }
}
