//! Разбор параметров командной строки. Здесь только разбор — без вывода и без доступа к системе,
//! поэтому всё легко проверяется тестами.

use crate::cleaner::Mode;

use super::UsageError;

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
            _ => return Err(UsageError::UnknownOption(arg.clone())),
        }
        first_run_option.get_or_insert_with(|| name.to_owned());
    }

    let command = match (mode, first_run_option) {
        (Some(_), _) => Command::Run(request),
        (None, Some(option)) => return Err(UsageError::OptionNotForCommand(option)),
        (None, None) if command_word.eq_ignore_ascii_case("list") => Command::List,
        (None, None) => Command::Help,
    };
    Ok(Arguments { command, language })
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
        assert_eq!(parse_line("list --quiet").unwrap_err(), UsageError::OptionNotForCommand("--quiet".into()));
    }
}
