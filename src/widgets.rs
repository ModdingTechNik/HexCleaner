//! Рисованные значки: щит администратора (как в UAC и Windows Terminal),
//! цветные точки уровней безопасности, круговая стрелка «вернуть из корзины»,
//! предупреждение «мимо корзины» и подписи, сокращённые посередине.

use eframe::egui::{
    Atom, Color32, Label, Painter, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, TextStyle, TextWrapMode, Ui,
    WidgetText, pos2, vec2,
};

const SHIELD_BLUE: Color32 = Color32::from_rgb(0x2D, 0x7D, 0xD2);
const SHIELD_YELLOW: Color32 = Color32::from_rgb(0xF5, 0xC5, 0x18);
const SHIELD_OUTLINE: Color32 = Color32::from_rgb(0x1B, 0x2A, 0x3D);

/// Щит как элемент виджета: его можно вставить в кнопку или подпись чекбокса,
/// например `Button::new((shield_atom(16.0), "Текст"))`.
pub fn shield_atom<'a>(height: f32) -> Atom<'a> {
    Atom::paint(vec2(height * 0.8, height), |ui, args| paint_shield(ui.painter(), args.rect))
}

/// Рисует щит, разделённый на четыре части: синие и жёлтые по диагонали.
pub fn paint_shield(painter: &Painter, rect: Rect) {
    let outline = shield_outline(rect);
    let center = rect.center();

    let quarters = [
        (Rect::from_min_max(rect.min, center), SHIELD_BLUE),
        (Rect::from_min_max(pos2(center.x, rect.top()), pos2(rect.right(), center.y)), SHIELD_YELLOW),
        (Rect::from_min_max(pos2(rect.left(), center.y), pos2(center.x, rect.bottom())), SHIELD_YELLOW),
        (Rect::from_min_max(center, rect.max), SHIELD_BLUE),
    ];
    for (quarter, color) in quarters {
        // Рисуем весь щит, но обрезанный по четверти — так получаем раскраску «шахматкой».
        let clip = quarter.intersect(painter.clip_rect());
        painter.with_clip_rect(clip).add(Shape::convex_polygon(outline.clone(), color, Stroke::NONE));
    }
    painter.add(Shape::closed_line(outline, Stroke::new(1.0, SHIELD_OUTLINE)));
}

/// Контур щита: покатые «плечи» сверху, прямые бока и закруглённый низ с остриём.
fn shield_outline(rect: Rect) -> Vec<Pos2> {
    let Rect { min, max } = rect.shrink(0.5);
    let center_x = (min.x + max.x) / 2.0;
    let shoulder_y = min.y + rect.height() * 0.12;
    let side_bottom_y = min.y + rect.height() * 0.55;

    let mut points = vec![pos2(min.x, shoulder_y), pos2(center_x, min.y), pos2(max.x, shoulder_y)];

    // Правая половина низа: кривая Безье от правого бока к острию.
    let right_curve = quadratic_bezier(pos2(max.x, side_bottom_y), pos2(max.x, max.y - 2.0), pos2(center_x, max.y));
    // Левая половина — зеркальная, от острия к левому боку.
    let left_curve = quadratic_bezier(pos2(center_x, max.y), pos2(min.x, max.y - 2.0), pos2(min.x, side_bottom_y));

    points.extend(right_curve);
    points.extend(left_curve.skip(1));
    points
}

fn quadratic_bezier(from: Pos2, control: Pos2, to: Pos2) -> impl Iterator<Item = Pos2> {
    const STEPS: usize = 8;
    (0..=STEPS).map(move |step| {
        let t = step as f32 / STEPS as f32;
        let a = from.to_vec2() * (1.0 - t) * (1.0 - t);
        let b = control.to_vec2() * 2.0 * (1.0 - t) * t;
        let c = to.to_vec2() * t * t;
        Pos2::ZERO + a + b + c
    })
}

/// Щит как самостоятельный значок (например, рядом с подписью «Администратор»).
pub fn shield_icon(ui: &mut Ui, height: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(height * 0.8, height), Sense::hover());
    paint_shield(ui.painter(), rect);
    response
}

/// Цветная точка как элемент виджета — метка уровня безопасности пункта.
pub fn dot_atom<'a>(color: Color32) -> Atom<'a> {
    Atom::paint(vec2(8.0, 8.0), move |ui, args| {
        ui.painter().circle_filled(args.rect.center(), 4.0, color);
    })
}

/// Цветная точка как самостоятельный значок (для легенды).
pub fn dot(ui: &mut Ui, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    response
}

/// Круговая стрелка против часовой стрелки — «вернуть из корзины». Цвет — как у текста кнопки.
pub fn undo_atom<'a>(size: f32) -> Atom<'a> {
    Atom::paint(vec2(size, size), |ui, args| {
        let color = ui.visuals().widgets.inactive.fg_stroke.color;
        paint_undo(ui.painter(), args.rect, color);
    })
}

fn paint_undo(painter: &Painter, rect: Rect, color: Color32) {
    use std::f32::consts::PI;
    let center = rect.center();
    let radius = rect.width() * 0.34;
    // Дуга в три четверти круга: от правого бока вверх, через левый бок вниз. Угол на экране
    // растёт по часовой стрелке, поэтому против часовой — от большего угла к меньшему.
    let (start, end) = (0.0_f32, -1.5 * PI);
    const STEPS: usize = 24;
    let points: Vec<Pos2> = (0..=STEPS)
        .map(|step| {
            let angle = start + (end - start) * step as f32 / STEPS as f32;
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    let width = (rect.width() * 0.11).max(1.2);
    painter.add(Shape::line(points, Stroke::new(width, color)));

    // Наконечник в конце дуги, по направлению движения.
    let tip_base = center + vec2(end.cos(), end.sin()) * radius;
    let direction = vec2(end.sin(), -end.cos());
    let normal = vec2(end.cos(), end.sin());
    let head = rect.width() * 0.22;
    let arrow = vec![
        tip_base + direction * head,
        tip_base + normal * head * 0.75,
        tip_base - normal * head * 0.75,
    ];
    painter.add(Shape::convex_polygon(arrow, color, Stroke::NONE));
}

/// Жёлтый треугольник с восклицательным знаком — «удаление мимо корзины».
pub fn warning_atom<'a>(size: f32) -> Atom<'a> {
    Atom::paint(vec2(size, size), |ui, args| paint_warning(ui.painter(), args.rect))
}

const WARNING_YELLOW: Color32 = Color32::from_rgb(0xF5, 0xC5, 0x18);
const WARNING_MARK: Color32 = Color32::from_rgb(0x20, 0x20, 0x20);

fn paint_warning(painter: &Painter, rect: Rect) {
    let rect = rect.shrink(0.5);
    let triangle = vec![pos2(rect.center().x, rect.top()), rect.right_bottom(), rect.left_bottom()];
    painter.add(Shape::convex_polygon(triangle, WARNING_YELLOW, Stroke::new(1.0, SHIELD_OUTLINE)));
    let x = rect.center().x;
    let height = rect.height();
    let width = (rect.width() * 0.12).max(1.5);
    painter.line_segment(
        [pos2(x, rect.top() + height * 0.35), pos2(x, rect.top() + height * 0.68)],
        Stroke::new(width, WARNING_MARK),
    );
    painter.circle_filled(pos2(x, rect.top() + height * 0.82), width * 0.65, WARNING_MARK);
}

/// Текст, сокращённый посередине до ширины `max_width`: `C:\Users\…\Cache\file.tmp`.
/// Начало и конец пути обычно важнее середины, поэтому конец сохраняется длиннее.
pub fn elide_middle(ui: &Ui, text: &str, max_width: f32) -> String {
    let width = |text: &str| {
        WidgetText::from(text).into_galley(ui, Some(TextWrapMode::Extend), f32::INFINITY, TextStyle::Body).size().x
    };
    if width(text) <= max_width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| {
        let head = keep * 2 / 5;
        let tail = keep - head;
        let mut result: String = chars[..head].iter().collect();
        result.push('…');
        result.extend(&chars[chars.len() - tail..]);
        result
    };
    // Сколько символов оставить — двоичным поиском по ширине.
    let (mut low, mut high) = (0, chars.len().saturating_sub(1));
    while low < high {
        let middle = (low + high).div_ceil(2);
        if width(&cut(middle)) <= max_width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    cut(low)
}

/// Подпись, сокращённая посередине до доступной ширины; полный текст — во всплывающей подсказке.
pub fn elided_label(ui: &mut Ui, text: impl Into<String>, strong: bool) -> Response {
    let text = text.into();
    let shown = elide_middle(ui, &text, ui.available_width());
    let mut rich = RichText::new(&shown);
    if strong {
        rich = rich.strong();
    }
    let response = ui.add(Label::new(rich).wrap_mode(TextWrapMode::Extend));
    if shown != text { response.on_hover_text(text) } else { response }
}
