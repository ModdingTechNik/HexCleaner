//! `--elevate`: анализ или очистка с правами администратора из обычной командной строки.
//!
//! Права в Windows выдаются только новому процессу, и консоль с ним не разделить. Поэтому
//! программа запускает свою копию с запросом UAC, без окна и со служебным `--relay <папка>`.
//! Копия пишет вывод в файлы этой папки, а исходная программа показывает их по мере появления,
//! передаёт Ctrl+C (файлом `stop` в той же папке) и возвращает код возврата копии.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use crate::system::admin::{self, ElevateError};
use crate::system::console;
use crate::t;

use super::ExitCode;

const STDOUT: &str = "stdout.txt";
const STDERR: &str = "stderr.txt";
const STOP: &str = "stop";

/// Как часто проверять, не появился ли новый вывод копии и не нажат ли Ctrl+C.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Файл, появление которого означает Ctrl+C в исходной программе (только у копии с правами).
static STOP_FILE: OnceLock<PathBuf> = OnceLock::new();

/// Нажат ли Ctrl+C — здесь или, для копии с правами, в исходной программе.
pub fn stop_requested() -> bool {
    console::ctrl_c_pressed() || STOP_FILE.get().is_some_and(|path| path.exists())
}

/// Эта программа — копия с правами: вывод — в файлы папки `dir`, остановка — по файлу в ней.
pub fn start_relay(dir: &Path) -> io::Result<()> {
    let stdout = File::create(dir.join(STDOUT))?;
    let stderr = File::create(dir.join(STDERR))?;
    console::redirect_output(stdout, stderr);
    let _ = STOP_FILE.set(dir.join(STOP));
    Ok(())
}

/// Выполняет команду `args` (уже без `--elevate`) с правами администратора и показывает её вывод.
/// `exe` — как запускать команды (см. `cli::run`): параметры после имени программы повторяются.
pub fn run(args: &[String], exe: &str, quiet: bool) -> ExitCode {
    let dir = std::env::temp_dir().join(format!("HexCleaner-elevate-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let code = match fs::create_dir_all(&dir) {
        Ok(()) => run_in(&dir, args, exe, quiet),
        Err(error) => {
            eprintln!("{}", t!("cli.elevate.failed", error = error));
            ExitCode::Failed
        }
    };
    let _ = fs::remove_dir_all(&dir);
    code
}

fn run_in(dir: &Path, args: &[String], exe: &str, quiet: bool) -> ExitCode {
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{}", t!("cli.elevate.failed", error = error));
            return ExitCode::Failed;
        }
    };
    if !quiet {
        println!("{}", t!("cli.elevate.starting"));
    }
    // Ctrl+C до ответа в окне UAC не обрывает программу: копия, если запустится, получит знак остановиться.
    console::catch_ctrl_c();
    let process = match admin::start_elevated(&program, &child_parameters(args, exe, dir)) {
        Ok(process) => process,
        Err(ElevateError::Cancelled) => {
            eprintln!("{}", t!("cli.elevate.cancelled"));
            return ExitCode::Failed;
        }
        Err(ElevateError::Failed(error)) => {
            eprintln!("{}", t!("cli.elevate.failed", error = error));
            return ExitCode::Failed;
        }
    };
    let code = relay(dir, |timeout| process.wait(timeout), console::ctrl_c_pressed, &mut io::stdout(), &mut io::stderr());
    exit_code(code)
}

/// Параметры копии: что было после имени программы в `exe`, команда без `--elevate` и `--relay <dir>`.
fn child_parameters(args: &[String], exe: &str, dir: &Path) -> String {
    let prefix = exe.split_whitespace().skip(1).map(str::to_owned);
    let command = args.iter().filter(|arg| *arg != "--elevate").cloned();
    let relay = ["--relay".to_owned(), dir.display().to_string()];
    prefix.chain(command).chain(relay).map(|arg| quote(&arg)).collect::<Vec<_>>().join(" ")
}

/// Параметр командной строки в кавычках, если нужно, — по правилам разбора Windows
/// (`CommandLineToArgvW`): обратные косые перед кавычкой и в конце удваиваются.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
        return arg.to_owned();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                continue;
            }
            '"' => quoted.push_str(&"\\".repeat(backslashes * 2 + 1)),
            _ => quoted.push_str(&"\\".repeat(backslashes)),
        }
        quoted.push(c);
        backslashes = 0;
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

/// Показывает вывод копии из файлов папки `dir`, пока она не завершится, и возвращает её код.
/// `wait` ждёт копию не дольше заданного времени и возвращает код, если она завершилась;
/// `stop_pressed` — нажат ли Ctrl+C: тогда копии один раз подаётся знак остановиться.
fn relay(
    dir: &Path,
    mut wait: impl FnMut(Duration) -> Option<u32>,
    stop_pressed: impl Fn() -> bool,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u32 {
    let (stdout_path, stderr_path) = (dir.join(STDOUT), dir.join(STDERR));
    let (mut stdout_offset, mut stderr_offset) = (0, 0);
    let mut stop_sent = false;
    loop {
        let finished = wait(POLL_INTERVAL);
        // После завершения копии — ещё раз: дочитать то, что она успела записать.
        copy_new_output(&stdout_path, &mut stdout_offset, stdout);
        copy_new_output(&stderr_path, &mut stderr_offset, stderr);
        if let Some(code) = finished {
            return code;
        }
        if !stop_sent && stop_pressed() {
            stop_sent = true;
            let _ = File::create(dir.join(STOP));
        }
    }
}

/// Дописывает в `output` то, что появилось в файле после `offset`.
fn copy_new_output(path: &Path, offset: &mut u64, output: &mut impl Write) {
    let Ok(mut file) = File::open(path) else { return };
    let mut new = Vec::new();
    if file.seek(SeekFrom::Start(*offset)).is_ok() && file.read_to_end(&mut new).is_ok() && !new.is_empty() {
        *offset += new.len() as u64;
        let _ = output.write_all(&new);
        let _ = output.flush();
    }
}

/// Код возврата копии — те же коды, что и у этой программы.
fn exit_code(code: u32) -> ExitCode {
    match code {
        0 => ExitCode::Success,
        1 => ExitCode::BadArguments,
        2 => ExitCode::PartiallyDone,
        4 => ExitCode::Cancelled,
        _ => ExitCode::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn quotes_arguments_like_windows_parses_them() {
        assert_eq!(quote("clean"), "clean");
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote("Раз в неделю"), r#""Раз в неделю""#);
        assert_eq!(quote(r"C:\Temp\My Dir\"), r#""C:\Temp\My Dir\\""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r#"a\"b"#), r#""a\\\"b""#);
    }

    #[test]
    fn child_repeats_prefix_and_drops_elevate() {
        let args: Vec<String> = ["clean", "--preset", "Раз в неделю", "--elevate", "--quiet"].map(str::to_owned).into();
        let dir = Path::new(r"C:\Temp\HexCleaner-elevate-1");
        assert_eq!(
            child_parameters(&args, "HexCleanerCli", dir),
            r#"clean --preset "Раз в неделю" --quiet --relay C:\Temp\HexCleaner-elevate-1"#
        );
        assert!(child_parameters(&args, "HexCleaner --scheduled", dir).starts_with("--scheduled clean "));
    }

    /// Вывод копии появляется по мере записи, Ctrl+C передаётся файлом, код возврата — её.
    #[test]
    fn relays_output_stop_and_exit_code() {
        let dir = std::env::temp_dir().join(format!("hexcleaner_relay_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Вместо копии с правами — шаги, которые она делает между проверками.
        let step = Cell::new(0);
        let wait = |_| {
            step.set(step.get() + 1);
            let append = |name: &str, text: &str| {
                let mut file = fs::OpenOptions::new().create(true).append(true).open(dir.join(name)).unwrap();
                file.write_all(text.as_bytes()).unwrap();
            };
            match step.get() {
                1 => None,
                2 => {
                    append(STDOUT, "[1/2] Пункт … ");
                    None
                }
                3 => {
                    append(STDOUT, "готово\n");
                    append(STDERR, "предупреждение\n");
                    None
                }
                _ if dir.join(STOP).exists() => {
                    append(STDOUT, "Остановлено\n");
                    Some(4)
                }
                _ => None,
            }
        };
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = relay(&dir, wait, || step.get() >= 3, &mut stdout, &mut stderr);

        assert_eq!(exit_code(code), ExitCode::Cancelled);
        assert_eq!(String::from_utf8(stdout).unwrap(), "[1/2] Пункт … готово\nОстановлено\n");
        assert_eq!(String::from_utf8(stderr).unwrap(), "предупреждение\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
