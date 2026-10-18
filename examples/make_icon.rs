//! Собирает иконку программы из SVG-исходников в `assets/`:
//!
//! * `assets/icon.ico` — все размеры для Windows (вшивается в exe, см. `build.rs`);
//! * `assets/icon-256.png` — иконка окна и панели задач (см. `src/main.rs`).
//!
//! Мелкие размеры рисуются из упрощённой `icon-small.svg`: в 16–24 px мелкие детали
//! сливаются в пятно. Как и в стандартных иконках Windows, размер 256 хранится в PNG,
//! а остальные — в классическом формате BMP: его понимают все программы, а PNG
//! в мелких размерах некоторые показывают мусором. Запуск после правки SVG:
//!
//! ```text
//! cargo run --example make_icon
//! ```

use std::fs;
use std::path::Path;

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

/// Размеры, которые Windows берёт из .ico: значки в списках, на панели задач,
/// в Проводнике при разных масштабах экрана.
const SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// До какого размера включительно используется упрощённый рисунок.
const SMALL_ICON_MAX_SIZE: u32 = 24;

fn main() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let full = load_svg(&assets.join("icon.svg"));
    let small = load_svg(&assets.join("icon-small.svg"));

    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&size| {
            let svg = if size <= SMALL_ICON_MAX_SIZE { &small } else { &full };
            let pixmap = render(svg, size);
            let data = if size >= 256 { pixmap.encode_png().unwrap() } else { ico_bitmap(&pixmap) };
            (size, data)
        })
        .collect();

    fs::write(assets.join("icon.ico"), ico_file(&images)).expect("не удалось записать icon.ico");
    let png_256 = render(&full, 256).encode_png().unwrap();
    fs::write(assets.join("icon-256.png"), png_256).expect("не удалось записать icon-256.png");

    println!("assets/icon.ico ({} размеров) и assets/icon-256.png готовы", images.len());
}

fn load_svg(path: &Path) -> Tree {
    let data = fs::read(path).unwrap_or_else(|error| panic!("не удалось прочитать {}: {error}", path.display()));
    Tree::from_data(&data, &Options::default()).unwrap_or_else(|error| panic!("ошибка в {}: {error}", path.display()))
}

fn render(svg: &Tree, size: u32) -> Pixmap {
    let mut pixmap = Pixmap::new(size, size).unwrap();
    let scale = size as f32 / svg.size().width();
    resvg::render(svg, Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap
}

/// Картинка для .ico в классическом формате: заголовок BITMAPINFOHEADER, пиксели BGRA
/// снизу вверх и однобитная маска прозрачности (нулевая — прозрачность берётся из альфы).
fn ico_bitmap(pixmap: &Pixmap) -> Vec<u8> {
    let (width, height) = (pixmap.width(), pixmap.height());
    let mask_row_bytes = width.div_ceil(32) * 4;

    let mut data = Vec::new();
    data.extend(40u32.to_le_bytes()); // размер заголовка
    data.extend((width as i32).to_le_bytes());
    data.extend((height as i32 * 2).to_le_bytes()); // высота картинки и маски вместе
    data.extend(1u16.to_le_bytes()); // плоскости
    data.extend(32u16.to_le_bytes()); // бит на пиксель
    data.extend([0u8; 24]); // без сжатия; остальные поля не нужны

    for row in (0..height).rev() {
        for column in 0..width {
            let color = pixmap.pixels()[(row * width + column) as usize].demultiply();
            data.extend([color.blue(), color.green(), color.red(), color.alpha()]);
        }
    }
    data.extend(vec![0u8; (mask_row_bytes * height) as usize]);
    data
}

/// Файл .ico из готовых картинок (BMP или PNG).
fn ico_file(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    const HEADER_SIZE: usize = 6;
    const ENTRY_SIZE: usize = 16;

    let mut ico = Vec::new();
    ico.extend(0u16.to_le_bytes()); // зарезервировано
    ico.extend(1u16.to_le_bytes()); // тип: иконка
    ico.extend((images.len() as u16).to_le_bytes());

    let mut offset = HEADER_SIZE + ENTRY_SIZE * images.len();
    for (size, data) in images {
        // Размер 256 записывается как 0 — в байт он не помещается.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        ico.extend([side, side, 0, 0]); // ширина, высота, палитра, зарезервировано
        ico.extend(1u16.to_le_bytes()); // плоскости
        ico.extend(32u16.to_le_bytes()); // бит на пиксель
        ico.extend((data.len() as u32).to_le_bytes());
        ico.extend((offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, data) in images {
        ico.extend(data);
    }
    ico
}
