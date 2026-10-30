//! Команда `config`: те же настройки, что на вкладке «Настройки» окна.
//!
//! * `config` или `config show` — все настройки с пояснениями;
//! * `config get <ключ>` — одно значение (удобно для сценариев);
//! * `config set <ключ> <значение>` или `config set <ключ>=<значение>`;
//! * `config reset <ключ> [<ключ> …]` — вернуть как по умолчанию.
//!
//! Пункт меню проводника хранится не в настройках, а в реестре: его меняет [`run`].
//! Разбор ключей и значений — без доступа к системе, он проверяется тестами.

use std::path::PathBuf;

use crate::catalog::{self, CleanupTask};
use crate::i18n;
use crate::settings::{Compression, Settings};
use crate::system::explorer_menu::{self, MenuState};
use crate::t;
use crate::themes;
use crate::trash::{MAX_KEEP_DAYS, TrashPolicy};

use super::{ExitCode, UsageError};

/// Что сделать с настройками.
#[derive(Debug, PartialEq)]
pub enum ConfigCommand {
    Show,
    Get(String),
    Set { key: String, value: String },
    Reset(Vec<String>),
}

/// Настройка.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Language,
    Theme,
    Notify,
    FlashTaskbar,
    LogsCompress,
    LogsCompression,
    TrashLimit,
    TrashKeepDays,
    TrashCompress,
    TrashCompression,
    TrashPath,
    /// Что делать с файлами пункта: `trash.policy.<id>`.
    TrashPolicy(&'static str),
    /// Пункт в контекстном меню проводника (в реестре, а не в файле настроек).
    ExplorerMenu,
    UpdatesCheck,
}

/// Ключи в том же порядке, что и разделы вкладки «Настройки».
const KEYS: [(&str, Key); 13] = [
    ("language", Key::Language),
    ("theme", Key::Theme),
    ("notify", Key::Notify),
    ("flash_taskbar", Key::FlashTaskbar),
    ("logs.compress", Key::LogsCompress),
    ("logs.compression", Key::LogsCompression),
    ("trash.limit", Key::TrashLimit),
    ("trash.keep_days", Key::TrashKeepDays),
    ("trash.compress", Key::TrashCompress),
    ("trash.compression", Key::TrashCompression),
    ("trash.path", Key::TrashPath),
    ("explorer_menu", Key::ExplorerMenu),
    ("updates.check_on_start", Key::UpdatesCheck),
];

const POLICY_PREFIX: &str = "trash.policy.";

const GB: u64 = 1024 * 1024 * 1024;

/// Пункт меню проводника: каким он должен стать.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Menu {
    Off,
    On,
    /// Виден только при щелчке правой кнопкой с Shift.
    ShiftOnly,
}

/// Что изменилось: файл настроек или пункт меню проводника.
#[derive(Debug, PartialEq)]
enum Change {
    Settings,
    Menu(Menu),
}

/// Выполняет команду `config`; `exe` — как запускать команды (для подсказок в ошибках).
pub fn run(command: &ConfigCommand, settings: &mut Settings, exe: &str) -> ExitCode {
    let tasks = catalog::all_tasks();
    let result = match command {
        ConfigCommand::Show => {
            show(settings, &tasks, &explorer_menu::state());
            return ExitCode::Success;
        }
        ConfigCommand::Get(name) => parse_key(name, &tasks).map(|key| {
            println!("{}", value(key, settings, &explorer_menu::state()));
            Vec::new()
        }),
        ConfigCommand::Set { key, value } => parse_key(key, &tasks)
            .and_then(|key| set(key, value, settings).map(|change| vec![(key, change)])),
        ConfigCommand::Reset(names) => names
            .iter()
            .map(|name| parse_key(name, &tasks).map(|key| (key, reset(key, settings))))
            .collect::<Result<Vec<_>, _>>(),
    };

    let changes = match result {
        Ok(changes) => changes,
        Err(error) => {
            eprintln!("{}", error.message(exe));
            return ExitCode::BadArguments;
        }
    };
    if changes.is_empty() {
        return ExitCode::Success;
    }

    if changes.iter().any(|(_, change)| *change == Change::Settings)
        && let Err(error) = settings.try_save()
    {
        eprintln!("{}", t!("cli.config.save_failed", path = Settings::path().display(), error = error));
        return ExitCode::Failed;
    }
    for (_, change) in &changes {
        if let Change::Menu(menu) = change
            && let Err(error) = update_menu(*menu)
        {
            eprintln!("{}", t!("settings.explorer_error", error = error));
            return ExitCode::Failed;
        }
    }

    let menu = explorer_menu::state();
    for (key, _) in &changes {
        println!("{} = {}", key_name(*key), value(*key, settings, &menu));
    }
    ExitCode::Success
}

/// Все настройки: ключ, значение и пояснение.
fn show(settings: &Settings, tasks: &[CleanupTask], menu: &MenuState) {
    let mut rows: Vec<(String, String, String)> =
        KEYS.iter().map(|&(name, key)| (name.to_owned(), value(key, settings, menu), description(key, settings))).collect();
    // Что делать с файлами пунктов: общая строка с подсказкой, затем пункты не «как обычно».
    let default_policy = policy_name(TrashPolicy::default()).to_owned();
    let insert_at = rows.iter().position(|(name, ..)| name == "explorer_menu").unwrap_or(rows.len());
    let mut policies = vec![(format!("{POLICY_PREFIX}<id>"), default_policy, t!("cli.config.trash_policy"))];
    for task in tasks.iter().filter(|task| task.deletes_files()) {
        let policy = settings.trash.policy(task.id);
        if policy != TrashPolicy::default() {
            policies.push((key_name(Key::TrashPolicy(task.id)), policy_name(policy).to_owned(), task.title()));
        }
    }
    rows.splice(insert_at..insert_at, policies);

    let key_width = rows.iter().map(|(key, ..)| key.chars().count()).max().unwrap_or(0);
    let value_width = rows.iter().map(|(_, value, _)| value.chars().count()).max().unwrap_or(0);
    println!("{}", t!("cli.config.file", path = Settings::path().display()));
    println!();
    for (key, value, description) in rows {
        println!("  {key:<key_width$}  {value:<value_width$}  {description}");
    }
}

fn key_name(key: Key) -> String {
    match key {
        Key::TrashPolicy(id) => format!("{POLICY_PREFIX}{id}"),
        _ => KEYS.iter().find(|(_, known)| *known == key).map(|(name, _)| (*name).to_owned()).unwrap_or_default(),
    }
}

fn parse_key(name: &str, tasks: &[CleanupTask]) -> Result<Key, UsageError> {
    let lower = name.trim().to_ascii_lowercase();
    if let Some(&(_, key)) = KEYS.iter().find(|(known, _)| *known == lower) {
        return Ok(key);
    }
    lower
        .strip_prefix(POLICY_PREFIX)
        .and_then(|id| tasks.iter().find(|task| task.id == id && task.deletes_files()))
        .map(|task| Key::TrashPolicy(task.id))
        .ok_or_else(|| UsageError::UnknownSetting(name.to_owned()))
}

/// Значение настройки в том виде, в котором его принимает `config set`.
fn value(key: Key, settings: &Settings, menu: &MenuState) -> String {
    let trash = &settings.trash;
    match key {
        Key::Language => settings.language.clone().unwrap_or_else(|| "system".to_owned()),
        Key::Theme => settings.theme.clone(),
        Key::Notify => on_off(settings.notify_when_done),
        Key::FlashTaskbar => on_off(settings.flash_taskbar),
        Key::LogsCompress => on_off(settings.compress_old_logs),
        Key::LogsCompression => compression_name(settings.log_compression).to_owned(),
        Key::TrashLimit => format_limit(trash.limit),
        Key::TrashKeepDays => trash.keep_days.map_or_else(|| "off".to_owned(), |days| days.to_string()),
        Key::TrashCompress => on_off(trash.compress),
        Key::TrashCompression => compression_name(trash.compression).to_owned(),
        Key::TrashPath => trash.path.as_ref().map_or_else(|| "default".to_owned(), |path| path.display().to_string()),
        Key::TrashPolicy(id) => policy_name(trash.policy(id)).to_owned(),
        Key::ExplorerMenu => match (menu.installed, menu.shift_only) {
            (false, _) => "off",
            (true, false) => "on",
            (true, true) => "shift",
        }
        .to_owned(),
        Key::UpdatesCheck => on_off(settings.updates.check_on_start),
    }
}

/// Пояснение к настройке для `config show`: что это и какие значения можно задать.
fn description(key: Key, settings: &Settings) -> String {
    match key {
        Key::Language => {
            let codes: Vec<String> = i18n::available_languages().0.into_iter().map(|language| language.code).collect();
            t!("cli.config.language", codes = codes.join(", "), system = i18n::system_language())
        }
        Key::Theme => t!("cli.config.theme", themes = theme_ids().join(", "), folder = themes::themes_directory().display()),
        Key::Notify => t!("cli.config.notify"),
        Key::FlashTaskbar => t!("cli.config.flash_taskbar"),
        Key::LogsCompress => t!("cli.config.logs_compress"),
        Key::LogsCompression => t!("cli.config.logs_compression"),
        Key::TrashLimit => t!("cli.config.trash_limit"),
        Key::TrashKeepDays => t!("cli.config.trash_keep_days", max = MAX_KEEP_DAYS),
        Key::TrashCompress => t!("cli.config.trash_compress"),
        Key::TrashCompression => t!("cli.config.trash_compression"),
        Key::TrashPath => t!("cli.config.trash_path", path = settings.trash.directory().display()),
        Key::TrashPolicy(_) => t!("cli.config.trash_policy"),
        Key::ExplorerMenu => t!("cli.config.explorer_menu", label = t!("explorer_menu.label")),
        Key::UpdatesCheck => t!("cli.config.updates_check"),
    }
}

/// Какие значения подходят — для сообщения об ошибке.
fn expected(key: Key) -> String {
    match key {
        Key::Language => {
            let mut codes: Vec<String> = i18n::available_languages().0.into_iter().map(|language| language.code).collect();
            codes.push("system".to_owned());
            codes.join(", ")
        }
        Key::Theme => {
            let mut ids = theme_ids();
            ids.push(themes::SYSTEM.to_owned());
            ids.join(", ")
        }
        Key::Notify | Key::FlashTaskbar | Key::LogsCompress | Key::TrashCompress | Key::UpdatesCheck => {
            "on, off".to_owned()
        }
        Key::LogsCompression | Key::TrashCompression => "ntfs, zip".to_owned(),
        Key::TrashLimit => "5GB, 500MB, 1.5GB, off, unlimited".to_owned(),
        Key::TrashKeepDays => format!("1–{MAX_KEEP_DAYS}, off"),
        Key::TrashPath => "<path>, default".to_owned(),
        Key::TrashPolicy(_) => "normal, always, never".to_owned(),
        Key::ExplorerMenu => "on, shift, off".to_owned(),
    }
}

/// Меняет настройку. Пункт меню проводника только разбирается — менять его будет [`run`].
fn set(key: Key, text: &str, settings: &mut Settings) -> Result<Change, UsageError> {
    let lower = text.trim().to_ascii_lowercase();
    let invalid =
        || UsageError::InvalidSetting { key: key_name(key), value: text.to_owned(), expected: expected(key) };
    let trash = &mut settings.trash;
    match key {
        Key::Language if lower == "system" => settings.language = None,
        Key::Language => {
            let languages = i18n::available_languages().0;
            let language = languages.iter().find(|language| language.code.eq_ignore_ascii_case(&lower)).ok_or_else(invalid)?;
            settings.language = Some(language.code.clone());
        }
        Key::Theme if lower == themes::SYSTEM => settings.theme = themes::SYSTEM.to_owned(),
        Key::Theme => {
            // Код темы — имя файла: в нём может быть и заглавная буква.
            let id = theme_ids().into_iter().find(|id| id.eq_ignore_ascii_case(&lower)).ok_or_else(invalid)?;
            settings.theme = id;
        }
        Key::Notify => settings.notify_when_done = parse_bool(&lower).ok_or_else(invalid)?,
        Key::FlashTaskbar => settings.flash_taskbar = parse_bool(&lower).ok_or_else(invalid)?,
        Key::LogsCompress => settings.compress_old_logs = parse_bool(&lower).ok_or_else(invalid)?,
        Key::LogsCompression => settings.log_compression = parse_compression(&lower).ok_or_else(invalid)?,
        Key::TrashLimit => trash.limit = parse_limit(&lower).ok_or_else(invalid)?,
        Key::TrashKeepDays => trash.keep_days = parse_days(&lower).ok_or_else(invalid)?,
        Key::TrashCompress => trash.compress = parse_bool(&lower).ok_or_else(invalid)?,
        Key::TrashCompression => trash.compression = parse_compression(&lower).ok_or_else(invalid)?,
        Key::TrashPath if lower == "default" => trash.path = None,
        Key::TrashPath => {
            let path = PathBuf::from(text.trim());
            if path.as_os_str().is_empty() {
                return Err(invalid());
            }
            // Относительный путь — от текущей папки: в настройках он должен значить то же самое.
            trash.path = Some(std::path::absolute(&path).unwrap_or(path));
        }
        Key::TrashPolicy(id) => trash.set_policy(id, parse_policy(&lower).ok_or_else(invalid)?),
        Key::ExplorerMenu => {
            let menu = match lower.as_str() {
                "shift" => Menu::ShiftOnly,
                _ if parse_bool(&lower).ok_or_else(invalid)? => Menu::On,
                _ => Menu::Off,
            };
            return Ok(Change::Menu(menu));
        }
        Key::UpdatesCheck => settings.updates.check_on_start = parse_bool(&lower).ok_or_else(invalid)?,
    }
    Ok(Change::Settings)
}

/// Возвращает настройке значение по умолчанию.
fn reset(key: Key, settings: &mut Settings) -> Change {
    let defaults = Settings::default();
    let trash = &mut settings.trash;
    match key {
        Key::Language => settings.language = defaults.language,
        Key::Theme => settings.theme = defaults.theme,
        Key::Notify => settings.notify_when_done = defaults.notify_when_done,
        Key::FlashTaskbar => settings.flash_taskbar = defaults.flash_taskbar,
        Key::LogsCompress => settings.compress_old_logs = defaults.compress_old_logs,
        Key::LogsCompression => settings.log_compression = defaults.log_compression,
        Key::TrashLimit => trash.limit = defaults.trash.limit,
        Key::TrashKeepDays => trash.keep_days = defaults.trash.keep_days,
        Key::TrashCompress => trash.compress = defaults.trash.compress,
        Key::TrashCompression => trash.compression = defaults.trash.compression,
        Key::TrashPath => trash.path = defaults.trash.path,
        Key::TrashPolicy(id) => trash.set_policy(id, TrashPolicy::default()),
        Key::ExplorerMenu => return Change::Menu(Menu::Off),
        Key::UpdatesCheck => settings.updates.check_on_start = defaults.updates.check_on_start,
    }
    Change::Settings
}

/// Добавляет, меняет или убирает пункт меню проводника. Пункт запускает окно программы —
/// `HexCleaner.exe` из той же папки, что и командная строка.
fn update_menu(menu: Menu) -> std::io::Result<()> {
    if menu == Menu::Off {
        return explorer_menu::uninstall();
    }
    let exe = std::env::current_exe()?.with_file_name("HexCleaner.exe");
    if !exe.is_file() {
        return Err(std::io::Error::other(t!("cli.config.no_gui_exe", path = exe.display())));
    }
    explorer_menu::install(&exe, &t!("explorer_menu.label"), menu == Menu::ShiftOnly)
}

fn on_off(value: bool) -> String {
    if value { "on" } else { "off" }.to_owned()
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "on" | "true" | "yes" | "1" => Some(true),
        "off" | "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// Коды тем: встроенных и из папки тем.
fn theme_ids() -> Vec<String> {
    themes::available_themes().0.into_iter().map(|theme| theme.id).collect()
}

fn compression_name(compression: Compression) -> &'static str {
    match compression {
        Compression::Ntfs => "ntfs",
        Compression::Zip => "zip",
    }
}

fn parse_compression(value: &str) -> Option<Compression> {
    [Compression::Ntfs, Compression::Zip].into_iter().find(|&compression| compression_name(compression) == value)
}

fn policy_name(policy: TrashPolicy) -> &'static str {
    match policy {
        TrashPolicy::Normal => "normal",
        TrashPolicy::Always => "always",
        TrashPolicy::Never => "never",
    }
}

fn parse_policy(value: &str) -> Option<TrashPolicy> {
    [TrashPolicy::Normal, TrashPolicy::Always, TrashPolicy::Never].into_iter().find(|&policy| policy_name(policy) == value)
}

/// Срок хранения: число дней или `off` (`None` — хранить, пока не вытеснит лимит).
fn parse_days(value: &str) -> Option<Option<u32>> {
    if value == "off" {
        return Some(None);
    }
    value.parse().ok().filter(|days| (1..=MAX_KEEP_DAYS).contains(days)).map(Some)
}

/// Лимит корзины: `off` — отключена (`Some(0)`), `unlimited` — без ограничений (`None`),
/// иначе размер: `5GB`, `500 MB`, `1,5G`; число без единиц — гигабайты, как в окне.
fn parse_limit(value: &str) -> Option<Option<u64>> {
    match value {
        "off" => return Some(Some(0)),
        "unlimited" => return Some(None),
        _ => {}
    }
    let value = value.replace(',', ".");
    let split = value.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number: f64 = number.parse().ok().filter(|number: &f64| number.is_finite())?;
    let power = match unit.trim() {
        "b" => 0,
        "k" | "kb" => 1,
        "m" | "mb" => 2,
        "" | "g" | "gb" => 3,
        "t" | "tb" => 4,
        _ => return None,
    };
    Some(Some((number * (1u64 << (10 * power)) as f64).round() as u64))
}

/// Лимит в том виде, в котором его можно снова задать: `5GB`, `1.5GB`, `700MB`.
fn format_limit(limit: Option<u64>) -> String {
    const MB: u64 = 1024 * 1024;
    match limit {
        None => "unlimited".to_owned(),
        Some(0) => "off".to_owned(),
        Some(bytes) if bytes % GB == 0 => format!("{}GB", bytes / GB),
        // Так пишет поле в окне: гигабайты с одним знаком после точки.
        Some(bytes) if ((bytes as f64 / GB as f64 * 10.0).round() / 10.0 * GB as f64).round() as u64 == bytes => {
            format!("{:.1}GB", bytes as f64 / GB as f64)
        }
        Some(bytes) if bytes % MB == 0 => format!("{}MB", bytes / MB),
        Some(bytes) => format!("{bytes}B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tasks() -> Vec<CleanupTask> {
        catalog::all_tasks()
    }

    fn set_text(settings: &mut Settings, key: &str, value: &str) -> Result<Change, UsageError> {
        set(parse_key(key, &tasks())?, value, settings)
    }

    #[test]
    fn every_key_shows_a_value_that_set_accepts() {
        let mut settings = Settings::default();
        settings.trash.keep_days = Some(14);
        settings.trash.limit = Some(GB / 2 * 3);
        settings.trash.path = Some(PathBuf::from(r"D:\Trash"));
        let menu = MenuState::default();
        for (name, key) in KEYS {
            let shown = value(key, &settings, &menu);
            let mut copy = Settings::default();
            assert!(set(key, &shown, &mut copy).is_ok(), "{name} = {shown} не принимается");
            if key != Key::ExplorerMenu && key != Key::Language {
                assert_eq!(value(key, &copy, &menu), shown, "{name}");
            }
        }
    }

    #[test]
    fn parses_keys_case_insensitively_and_task_policies() {
        assert_eq!(parse_key("Trash.Limit", &tasks()), Ok(Key::TrashLimit));
        assert_eq!(parse_key("trash.policy.user_temp", &tasks()), Ok(Key::TrashPolicy("user_temp")));
        assert!(parse_key("trash.policy.no_such_task", &tasks()).is_err());
        // Очистка памяти файлов не удаляет — корзины у неё нет.
        assert!(parse_key("trash.policy.memory", &tasks()).is_err());
        assert_eq!(parse_key("colour", &tasks()), Err(UsageError::UnknownSetting("colour".into())));
    }

    #[test]
    fn sets_values() {
        let mut settings = Settings::default();
        assert_eq!(set_text(&mut settings, "theme", "Light"), Ok(Change::Settings));
        assert_eq!(settings.theme, "light");
        set_text(&mut settings, "theme", "system").unwrap();
        assert_eq!(settings.theme, "system");
        set_text(&mut settings, "notify", "off").unwrap();
        assert!(!settings.notify_when_done);
        set_text(&mut settings, "logs.compression", "ntfs").unwrap();
        assert_eq!(settings.log_compression, Compression::Ntfs);
        set_text(&mut settings, "trash.keep_days", "30").unwrap();
        assert_eq!(settings.trash.keep_days, Some(30));
        set_text(&mut settings, "trash.policy.user_temp", "never").unwrap();
        assert_eq!(settings.trash.policy("user_temp"), TrashPolicy::Never);
        set_text(&mut settings, "trash.policy.user_temp", "normal").unwrap();
        assert!(settings.trash.policies.is_empty());
        set_text(&mut settings, "language", "EN").unwrap();
        assert_eq!(settings.language.as_deref(), Some("en"));
        set_text(&mut settings, "language", "system").unwrap();
        assert_eq!(settings.language, None);
        assert_eq!(set_text(&mut settings, "explorer_menu", "shift"), Ok(Change::Menu(Menu::ShiftOnly)));
        assert_eq!(set_text(&mut settings, "explorer_menu", "no"), Ok(Change::Menu(Menu::Off)));
    }

    #[test]
    fn rejects_invalid_values() {
        let mut settings = Settings::default();
        for (key, value) in [
            ("theme", "no-such-theme-for-tests"),
            ("notify", "maybe"),
            ("trash.limit", "5 parsecs"),
            ("trash.keep_days", "0"),
            ("trash.keep_days", "99999"),
            ("trash.path", " "),
            ("language", "no-such-language-for-tests"),
            ("explorer_menu", "sometimes"),
        ] {
            assert!(
                matches!(set_text(&mut settings, key, value), Err(UsageError::InvalidSetting { .. })),
                "{key} = {value} принято"
            );
        }
    }

    #[test]
    fn parses_and_formats_trash_limits() {
        assert_eq!(parse_limit("off"), Some(Some(0)));
        assert_eq!(parse_limit("unlimited"), Some(None));
        assert_eq!(parse_limit("5"), Some(Some(5 * GB)));
        assert_eq!(parse_limit("5gb"), Some(Some(5 * GB)));
        assert_eq!(parse_limit("500 mb"), Some(Some(500 * 1024 * 1024)));
        assert_eq!(parse_limit("1,5g"), Some(Some(GB / 2 * 3)));
        assert_eq!(parse_limit("-1gb"), None);
        assert_eq!(parse_limit("gb"), None);

        assert_eq!(format_limit(Some(5 * GB)), "5GB");
        assert_eq!(format_limit(Some(GB / 2 * 3)), "1.5GB");
        assert_eq!(format_limit(Some(700 * 1024 * 1024)), "700MB");
        assert_eq!(format_limit(Some(12345)), "12345B");
        // Как округляет поле в окне: 0.1 ГБ не делится на мегабайты нацело.
        let tenth = (0.1 * GB as f64).round() as u64;
        assert_eq!(format_limit(Some(tenth)), "0.1GB");
        assert_eq!(parse_limit(&format_limit(Some(tenth)).to_ascii_lowercase()), Some(Some(tenth)));
    }

    #[test]
    fn reset_restores_defaults() {
        let mut settings = Settings::default();
        set_text(&mut settings, "trash.limit", "unlimited").unwrap();
        set_text(&mut settings, "trash.policy.user_temp", "always").unwrap();
        reset(Key::TrashLimit, &mut settings);
        reset(Key::TrashPolicy("user_temp"), &mut settings);
        assert_eq!(settings.trash, Settings::default().trash);
        assert_eq!(reset(Key::ExplorerMenu, &mut settings), Change::Menu(Menu::Off));
    }
}
