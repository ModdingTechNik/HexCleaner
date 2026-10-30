//! Разбор параметров командной строки. Здесь только разбор — без вывода и без доступа к системе,
//! поэтому всё легко проверяется тестами.

use crate::cleaner::Mode;

use std::path::PathBuf;

use super::UsageError;
use super::config::ConfigCommand;
use super::trash::TrashCommand;

/// Что запросили в командной строке.
#[derive(Debug, PartialEq)]
pub struct Arguments {
    pub command: Command,
    /// Язык вывода и журнала (`--lang en`), если указан.
    pub language: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Command {
    /// `analyze` или `clean`.
    Run(RunRequest),
    List,
    /// `config`: показать или изменить настройки.
    Config(ConfigCommand),
    /// `trash`: корзина HexCleaner.
    Trash(TrashCommand),
    Help,
}

/// Параметры анализа или очистки.
#[derive(Debug, PartialEq)]
pub struct RunRequest {
    pub mode: Mode,
    pub choice: TaskChoice,
    /// `--details`: показать папки, в которых найден мусор.
    pub details: bool,
    /// `--quiet`: печатать только итог.
    pub quiet: bool,
    /// `--compress-logs` / `--no-compress-logs`; `None` — как настроено в программе.
    pub compress_logs: Option<bool>,
    /// `--close-apps`: перед очисткой закрыть программы, которые держат файлы выбранных пунктов.
    /// При анализе — только показать их.
    pub close_apps: bool,
    /// `--restart-apps`: после очистки запустить закрытые программы снова.
    pub restart_apps: bool,
    /// `--no-trash`: удалять сразу, мимо корзины.
    pub no_trash: bool,
    /// `--elevate`: выполнить с правами администратора (запрос UAC), вывод — здесь же.
    pub elevate: bool,
    /// Служебный `--relay <папка>`: эта программа — копия, запущенная по `--elevate`;
    /// вывод — в файлы этой папки (см. `cli::elevate`). В справке его нет.
    pub relay: Option<PathBuf>,
}

/// Какие пункты обработать. Наборы, пункты и `--selected` складываются,
/// затем из результата убираются пункты из `--exclude`.
#[derive(Debug, Default, PartialEq)]
pub struct TaskChoice {
    pub presets: Vec<String>,
    pub tasks: Vec<String>,
    pub exclude: Vec<String>,
    /// Пункты, отмеченные в окне программы.
    pub selected: bool,
}

impl TaskChoice {
    /// Ничего не указано — тогда берутся пункты, отмеченные в окне программы.
    pub fn is_empty(&self) -> bool {
        self.presets.is_empty() && self.tasks.is_empty() && !self.selected
    }
}

pub fn parse(args: &[String]) -> Result<Arguments, UsageError> {
    let mut args = args.iter();
    let Some(command_word) = args.next() else {
        return Ok(Arguments { command: Command::Help, language: None });
    };

    if command_word.eq_ignore_ascii_case("config") {
        return parse_config(args);
    }
    if command_word.eq_ignore_ascii_case("trash") {
        return parse_trash(args);
    }
    let mode = match command_word.to_ascii_lowercase().as_str() {
        "analyze" => Some(Mode::Analyze),
        "clean" => Some(Mode::Clean),
        "list" | "help" | "-h" | "--help" | "/?" => None,
        _ => return Err(UsageError::UnknownCommand(command_word.clone())),
    };
    let mut request = RunRequest {
        mode: mode.unwrap_or(Mode::Analyze),
        choice: TaskChoice::default(),
        details: false,
        quiet: false,
        compress_logs: None,
        close_apps: false,
        restart_apps: false,
        no_trash: false,
        elevate: false,
        relay: None,
    };
    let mut language = None;
    // Первый параметр анализа или очистки — чтобы сообщить об ошибке, если он указан у list или help.
    let mut first_run_option = None;

    while let Some(arg) = args.next() {
        // Значение можно писать и через пробел, и через «=»: `--preset safe`, `--preset=safe`.
        let (name, inline_value) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = || {
            inline_value.clone().or_else(|| args.next().cloned()).ok_or_else(|| UsageError::MissingValue(name.to_owned()))
        };

        match name {
            "--lang" => {
                language = Some(value()?);
                continue;
            }
            "--preset" => request.choice.presets.push(value()?),
            "--tasks" => request.choice.tasks.extend(split_list(&value()?)),
            "--exclude" => request.choice.exclude.extend(split_list(&value()?)),
            "--selected" => request.choice.selected = true,
            "--details" => request.details = true,
            "--quiet" | "-q" => request.quiet = true,
            "--compress-logs" => request.compress_logs = Some(true),
            "--no-compress-logs" => request.compress_logs = Some(false),
            "--close-apps" => request.close_apps = true,
            "--restart-apps" => request.restart_apps = true,
            "--no-trash" => request.no_trash = true,
            "--elevate" => request.elevate = true,
            "--relay" => request.relay = Some(PathBuf::from(value()?)),
            _ => return Err(UsageError::UnknownOption(arg.clone())),
        }
        first_run_option.get_or_insert_with(|| name.to_owned());
    }

    let command = match (mode, first_run_option) {
        (Some(_), _) => Command::Run(request),
        (None, Some(option)) => {
            return Err(UsageError::OptionNotForCommand { option, command: command_word.to_ascii_lowercase() });
        }
        (None, None) if command_word.eq_ignore_ascii_case("list") => Command::List,
        (None, None) => Command::Help,
    };
    Ok(Arguments { command, language })
}

/// `config [show | get <ключ> | set <ключ> <значение> | reset <ключ>…] [--lang <код>]`.
fn parse_config<'a>(mut args: impl Iterator<Item = &'a String>) -> Result<Arguments, UsageError> {
    let mut words = Vec::new();
    let mut language = None;
    while let Some(arg) = args.next() {
        if let Some(code) = arg.strip_prefix("--lang=") {
            language = Some(code.to_owned());
        } else if arg == "--lang" {
            language = Some(args.next().cloned().ok_or_else(|| UsageError::MissingValue(arg.clone()))?);
        } else if arg.starts_with("--") {
            return Err(UsageError::UnknownOption(arg.clone()));
        } else {
            words.push(arg.clone());
        }
    }

    let mut words = words.into_iter();
    let action = words.next().unwrap_or_else(|| "show".to_owned());
    let mut word = |after: &str| words.next().ok_or_else(|| UsageError::MissingValue(after.to_owned()));
    let command = match action.to_ascii_lowercase().as_str() {
        "show" => ConfigCommand::Show,
        "get" => ConfigCommand::Get(word("get")?),
        // И `set trash.limit 10GB`, и `set trash.limit=10GB`.
        "set" => {
            let key = word("set")?;
            match key.split_once('=') {
                Some((key, value)) => ConfigCommand::Set { key: key.to_owned(), value: value.to_owned() },
                None => {
                    let value = word(&key)?;
                    ConfigCommand::Set { key, value }
                }
            }
        }
        "reset" => {
            let first = word("reset")?;
            ConfigCommand::Reset(std::iter::once(first).chain(words.by_ref()).collect())
        }
        _ => return Err(UsageError::UnknownCommand(format!("config {action}"))),
    };
    // Лишнее слово — скорее всего, путь с пробелами без кавычек.
    if let Some(extra) = words.next() {
        return Err(UsageError::UnknownOption(extra));
    }
    Ok(Arguments { command: Command::Config(command), language })
}

/// `trash [list | show <очистка> [--files] | restore <очистка> [--tasks <id,…>] [--path <путь>]…
/// | delete <очистка> --yes | empty --yes] [--lang <код>]`.
fn parse_trash<'a>(mut args: impl Iterator<Item = &'a String>) -> Result<Arguments, UsageError> {
    let mut words = Vec::new();
    let mut language = None;
    let (mut files, mut confirmed) = (false, false);
    let (mut tasks, mut paths) = (Vec::new(), Vec::new());
    // Указанные параметры — чтобы сообщить об ошибке, если параметр не подходит к действию.
    let mut options = Vec::new();

    while let Some(arg) = args.next() {
        let (name, inline_value) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = || {
            inline_value.clone().or_else(|| args.next().cloned()).ok_or_else(|| UsageError::MissingValue(name.to_owned()))
        };
        match name {
            "--lang" => {
                language = Some(value()?);
                continue;
            }
            "--files" => files = true,
            "--yes" | "-y" => confirmed = true,
            "--tasks" => tasks.extend(split_list(&value()?)),
            "--path" => paths.push(PathBuf::from(value()?)),
            _ if name.starts_with('-') => return Err(UsageError::UnknownOption(arg.clone())),
            _ => {
                words.push(arg.clone());
                continue;
            }
        }
        options.push(name.to_owned());
    }

    let mut words = words.into_iter();
    let action = words.next().unwrap_or_else(|| "list".to_owned()).to_ascii_lowercase();
    // Имя очистки с пробелом можно не брать в кавычки: всё после действия — это оно.
    let run = words.collect::<Vec<_>>().join(" ");
    let needs_run = |run: String| if run.is_empty() { Err(UsageError::MissingValue(action.clone())) } else { Ok(run) };
    let (command, allowed): (TrashCommand, &[&str]) = match action.as_str() {
        "list" => (TrashCommand::List, &[]),
        "show" => (TrashCommand::Show { run: needs_run(run.clone())?, files }, &["--files"]),
        "restore" => (TrashCommand::Restore { run: needs_run(run.clone())?, tasks, paths }, &["--tasks", "--path"]),
        "delete" => (TrashCommand::Delete { run: needs_run(run.clone())?, confirmed }, &["--yes", "-y"]),
        "empty" => (TrashCommand::Empty { confirmed }, &["--yes", "-y"]),
        _ => return Err(UsageError::UnknownCommand(format!("trash {action}"))),
    };
    if matches!(command, TrashCommand::List | TrashCommand::Empty { .. }) && !run.is_empty() {
        return Err(UsageError::UnknownOption(run));
    }
    if let Some(option) = options.into_iter().find(|option| !allowed.contains(&option.as_str())) {
        return Err(UsageError::OptionNotForCommand { option, command: format!("trash {action}") });
    }
    Ok(Arguments { command: Command::Trash(command), language })
}

/// `"a, b,,c"` → `["a", "b", "c"]`.
fn split_list(value: &str) -> impl Iterator<Item = String> + '_ {
    value.split(',').map(str::trim).filter(|item| !item.is_empty()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_line(line: &str) -> Result<Arguments, UsageError> {
        let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        parse(&args)
    }

    fn run_request(line: &str) -> RunRequest {
        match parse_line(line).unwrap().command {
            Command::Run(request) => request,
            other => panic!("ожидалась команда analyze или clean, получено {other:?}"),
        }
    }

    #[test]
    fn no_arguments_and_help_show_help() {
        assert_eq!(parse_line("").unwrap().command, Command::Help);
        assert_eq!(parse_line("help").unwrap().command, Command::Help);
        assert_eq!(parse_line("/?").unwrap().command, Command::Help);
        assert_eq!(parse_line("LIST").unwrap().command, Command::List);
    }

    #[test]
    fn parses_presets_tasks_and_flags() {
        let request = run_request("clean --preset safe --tasks recycle_bin,user_temp --exclude=browsers_cache --quiet");
        assert_eq!(request.mode, Mode::Clean);
        assert_eq!(request.choice.presets, ["safe"]);
        assert_eq!(request.choice.tasks, ["recycle_bin", "user_temp"]);
        assert_eq!(request.choice.exclude, ["browsers_cache"]);
        assert!(request.quiet && !request.details);
        assert_eq!(request.compress_logs, None);

        let request = run_request("analyze --details --no-compress-logs");
        assert_eq!(request.mode, Mode::Analyze);
        assert!(request.choice.is_empty());
        assert!(request.details);
        assert_eq!(request.compress_logs, Some(false));
        assert!(!request.close_apps && !request.restart_apps);

        let request = run_request("clean --preset safe --close-apps --restart-apps");
        assert!(request.close_apps && request.restart_apps);
        assert!(!request.no_trash && !request.elevate && request.relay.is_none());
        assert!(run_request("clean --no-trash").no_trash);
        let request = run_request(r"analyze --elevate --relay C:\Temp\x");
        assert!(request.elevate);
        assert_eq!(request.relay, Some(PathBuf::from(r"C:\Temp\x")));
    }

    fn config(line: &str) -> ConfigCommand {
        match parse_line(line).unwrap().command {
            Command::Config(command) => command,
            other => panic!("ожидалась команда config, получено {other:?}"),
        }
    }

    #[test]
    fn parses_config_commands() {
        assert_eq!(config("config"), ConfigCommand::Show);
        assert_eq!(config("CONFIG show"), ConfigCommand::Show);
        assert_eq!(config("config get trash.limit"), ConfigCommand::Get("trash.limit".into()));
        let set = ConfigCommand::Set { key: "trash.limit".into(), value: "10GB".into() };
        assert_eq!(config("config set trash.limit 10GB"), set);
        assert_eq!(config("config set trash.limit=10GB"), set);
        assert_eq!(config("config reset theme notify"), ConfigCommand::Reset(vec!["theme".into(), "notify".into()]));

        let arguments = parse_line("config --lang en set theme light").unwrap();
        assert_eq!(arguments.language.as_deref(), Some("en"));
        let set = ConfigCommand::Set { key: "theme".into(), value: "light".into() };
        assert_eq!(arguments.command, Command::Config(set));
    }

    fn trash(line: &str) -> TrashCommand {
        match parse_line(line).unwrap().command {
            Command::Trash(command) => command,
            other => panic!("ожидалась команда trash, получено {other:?}"),
        }
    }

    #[test]
    fn parses_trash_commands() {
        assert_eq!(trash("trash"), TrashCommand::List);
        assert_eq!(trash("trash list --lang en"), TrashCommand::List);
        assert_eq!(trash("trash show 1 --files"), TrashCommand::Show { run: "1".into(), files: true });
        // Имя очистки с пробелом — без кавычек.
        assert_eq!(
            trash("trash restore 2026-10-29 10-56-42 --tasks user_temp,browsers_cache --path C:\\Temp"),
            TrashCommand::Restore {
                run: "2026-10-29 10-56-42".into(),
                tasks: vec!["user_temp".into(), "browsers_cache".into()],
                paths: vec![PathBuf::from("C:\\Temp")],
            }
        );
        assert_eq!(trash("trash delete latest"), TrashCommand::Delete { run: "latest".into(), confirmed: false });
        assert_eq!(trash("trash delete latest --yes"), TrashCommand::Delete { run: "latest".into(), confirmed: true });
        assert_eq!(trash("trash empty -y"), TrashCommand::Empty { confirmed: true });
    }

    #[test]
    fn reports_trash_usage_errors() {
        assert_eq!(parse_line("trash wipe").unwrap_err(), UsageError::UnknownCommand("trash wipe".into()));
        assert_eq!(parse_line("trash show").unwrap_err(), UsageError::MissingValue("show".into()));
        assert_eq!(parse_line("trash restore 1 --path").unwrap_err(), UsageError::MissingValue("--path".into()));
        assert_eq!(parse_line("trash empty now").unwrap_err(), UsageError::UnknownOption("now".into()));
        assert_eq!(parse_line("trash restore 1 --force").unwrap_err(), UsageError::UnknownOption("--force".into()));
        assert_eq!(
            parse_line("trash show 1 --yes").unwrap_err(),
            UsageError::OptionNotForCommand { option: "--yes".into(), command: "trash show".into() }
        );
    }

    #[test]
    fn reports_config_usage_errors() {
        assert_eq!(parse_line("config wipe").unwrap_err(), UsageError::UnknownCommand("config wipe".into()));
        assert_eq!(parse_line("config set").unwrap_err(), UsageError::MissingValue("set".into()));
        assert_eq!(parse_line("config set theme").unwrap_err(), UsageError::MissingValue("theme".into()));
        assert_eq!(parse_line("config reset").unwrap_err(), UsageError::MissingValue("reset".into()));
        // Путь с пробелами без кавычек.
        assert_eq!(parse_line(r"config set trash.path D:\My Trash").unwrap_err(), UsageError::UnknownOption("Trash".into()));
        assert_eq!(parse_line("config show --quiet").unwrap_err(), UsageError::UnknownOption("--quiet".into()));
    }

    #[test]
    fn language_works_with_every_command() {
        let arguments = parse_line("list --lang en").unwrap();
        assert_eq!(arguments.command, Command::List);
        assert_eq!(arguments.language.as_deref(), Some("en"));
    }

    #[test]
    fn reports_usage_errors() {
        assert_eq!(parse_line("wipe").unwrap_err(), UsageError::UnknownCommand("wipe".into()));
        assert_eq!(parse_line("clean --force").unwrap_err(), UsageError::UnknownOption("--force".into()));
        assert_eq!(parse_line("clean --preset").unwrap_err(), UsageError::MissingValue("--preset".into()));
        assert_eq!(
            parse_line("list --quiet").unwrap_err(),
            UsageError::OptionNotForCommand { option: "--quiet".into(), command: "list".into() }
        );
    }
}
