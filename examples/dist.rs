//! Собирает релиз: `cargo build --release`, затем `HexCleaner.exe` и `HexCleanerCli.exe`
//! упаковываются в `target/release/release.zip` — его прикладывают к релизу на GitHub,
//! оттуда его скачивает обновление (см. `src/update.rs`). Запуск:
//!
//! ```text
//! cargo dist
//! ```
//!
//! `dist` — псевдоним из `.cargo/config.toml` для `cargo run --release --example dist`.
//! Отладочная сборка примера (например, запуск из RustRover) сама перезапускается в release:
//! без оптимизации Zopfli сжимает в разы дольше.
//!
//! Сжатие — Deflate через Zopfli: обычный zip, который распаковывает любая программа и само
//! обновление (оно умеет только Deflate), но заметно меньше, чем у zlib на максимуме, —
//! как режим «Ультра» у 7-Zip для zip. LZMA сжал бы сильнее, но такой архив обновление
//! не распакует.

use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::process::{Command, ExitCode};
use std::time::Instant;

use chrono::{DateTime, Local};
use hexcleaner::cleaner::format_size;
use hexcleaner::system::hash;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const EXES: [&str; 2] = ["HexCleaner.exe", "HexCleanerCli.exe"];
const ARCHIVE: &str = "release.zip";

/// Уровень Deflate: выше 9 — Zopfli с (уровень − 9) итерациями. 15 — как у самого Zopfli
/// по умолчанию: дальше архив почти не уменьшается, а время растёт.
const LEVEL: i64 = 9 + 15;

/// Сколько данных отдавать Zopfli за раз: столько он и так сжимает одним куском.
const CHUNK: usize = 1_000_000;

fn main() -> ExitCode {
    // Настройкой профиля Zopfli не ускорить: его кодировщик обобщён по типу вывода
    // и компилируется вместе с этим примером.
    if cfg!(debug_assertions) {
        println!("Отладочная сборка сжимала бы в разы дольше — перезапускаюсь в release…");
        return match cargo().args(["run", "--release", "--example", "dist"]).status() {
            Ok(status) if status.success() => ExitCode::SUCCESS,
            Ok(_) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("Ошибка: {error}");
                ExitCode::FAILURE
            }
        };
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Ошибка: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `cargo` в папке проекта. `CARGO` задаёт сам cargo при `cargo run`: тот же cargo и тот же тулчейн.
fn cargo() -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command
}

fn run() -> Result<(), Box<dyn Error>> {
    let status = cargo().args(["build", "--release", "--bins"]).status()?;
    if !status.success() {
        return Err(format!("cargo build --release завершился с ошибкой ({status})").into());
    }

    // Сам пример лежит в `target/<профиль>/examples/`, программы — в `target/release/`,
    // даже если пример собран в отладочном профиле или папка сборки перенесена через CARGO_TARGET_DIR.
    let exe = std::env::current_exe()?;
    let target = exe.ancestors().nth(3).ok_or("не найдена папка target")?;
    let release = target.join("release");
    let archive = release.join(ARCHIVE);

    println!("Упаковываю {} в {} (Zopfli, это займёт время)…", EXES.join(" и "), archive.display());
    let started = Instant::now();
    let files: Vec<(&str, Vec<u8>, DateTime<Local>)> = EXES
        .iter()
        .map(|name| {
            let path = release.join(name);
            let modified = fs::metadata(&path)?.modified()?;
            Ok((*name, fs::read(&path)?, modified.into()))
        })
        .collect::<Result<_, std::io::Error>>()?;

    // Пишем во временный файл: прерванная упаковка не оставит битый release.zip.
    let partial = release.join(format!("{ARCHIVE}.partial"));
    let mut zip = ZipWriter::new(File::create(&partial)?);
    for (name, data, modified) in &files {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(LEVEL))
            .last_modified_time(zip::DateTime::try_from(modified.naive_local())?);
        zip.start_file(*name, options)?;
        // Zopfli сжимает каждую запись отдельной порцией (с окном из предыдущей),
        // поэтому пишем по мегабайту — так виден ход упаковки.
        for (index, chunk) in data.chunks(CHUNK).enumerate() {
            zip.write_all(chunk)?;
            print!("\r  {name}: {} %", index * CHUNK * 100 / data.len());
            std::io::stdout().flush()?;
        }
        println!("\r  {name}: 100 %");
    }
    zip.finish()?;
    fs::rename(&partial, &archive)?;

    // Проверка: архив читается так же, как его прочтёт обновление, и файлы совпадают.
    let mut check = ZipArchive::new(File::open(&archive)?)?;
    for (name, data, _) in &files {
        let mut unpacked = Vec::with_capacity(data.len());
        check.by_name(name)?.read_to_end(&mut unpacked)?;
        if &unpacked != data {
            return Err(format!("{name} в архиве не совпадает с собранным").into());
        }
    }

    let packed = fs::read(&archive)?;
    let original: usize = files.iter().map(|(_, data, _)| data.len()).sum();
    println!(
        "Готово за {:.0} с: {} — {} из {} ({:.0} %)",
        started.elapsed().as_secs_f64(),
        archive.display(),
        format_size(packed.len() as u64),
        format_size(original as u64),
        packed.len() as f64 / original as f64 * 100.0,
    );
    if let Some(sha256) = hash::sha256_hex(&packed) {
        println!("SHA-256: {sha256}");
    }
    Ok(())
}
