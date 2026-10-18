//! Рисованные значки: щит администратора (как в UAC и Windows Terminal)
//! и цветные точки уровней безопасности.

use eframe::egui::{Atom, Color32, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};

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
