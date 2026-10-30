//! Каталог пунктов очистки.
//!
//! Каждый пункт описывается декларативно: идентификатор, нужны ли права администратора
//! и список простых действий ([`Action`]). Заголовок и описание пункта берутся из файлов
//! перевода по ключам `task.<id>.title` и `task.<id>.description`. Движок очистки (`cleaner`) и интерфейс
//! (`app`) ничего не знают о конкретных путях — чтобы добавить новый пункт,
//! достаточно дописать его в [`built_in_tasks`].
//!
//! Свои пункты пользователь описывает в JSON-файлах (см. [`crate::custom_tasks`]):
//! они идут в списке после встроенных, тексты у них — из самих файлов.
//!
//! Пункты разложены по категориям ([`Category`]): в окне каждая категория — сворачиваемая
//! группа. Свой пункт может попасть во встроенную категорию или завести свою.
//!
//! Пути записываются шаблонами:
//! * `%NAME%` — переменная окружения (`%LOCALAPPDATA%`, `%WINDIR%` ...)
//!   или специальная переменная (см. `cleaner::paths`), например `%STEAM%`;
//! * `*` — любое имя файла или папки (например, любой профиль браузера).
//!
//! У каждого пункта есть уровень безопасности ([`Safety`]): временные файлы и кэши
//! безопасны, остальное — с последствиями разной тяжести.
//!
//! Принцип: трогаем только кэши, временные файлы, журналы и отчёты.
//! Документы, сохранения игр, cookies, пароли и история браузеров не затрагиваются.

use std::time::Duration;

use crate::custom_tasks;
use crate::i18n::{self, LocalizedText};
use crate::t;

/// Временные файлы моложе этого возраста не удаляются:
/// ими может прямо сейчас пользоваться работающий установщик или программа.
const TEMP_MIN_AGE: Duration = Duration::from_secs(60 * 60);

/// Одно действие внутри пункта очистки.
pub enum Action {
    /// Удалить всё содержимое папок, подходящих под шаблон. Сами папки остаются.
    ClearDirectory { pattern: String, min_age: Option<Duration> },
    /// Удалить папки, подходящие под шаблон, вместе с содержимым.
    DeleteDirectory { pattern: String },
    /// Удалить файлы, подходящие под шаблон (например, `...\thumbcache_*.db`).
    DeleteFiles { pattern: String },
    /// Очистить все журналы событий Windows.
    ClearEventLogs,
    /// Сбросить рабочие наборы памяти всех процессов.
    TrimProcessMemory,
    /// Очистить корзину текущего пользователя на всех дисках.
    EmptyRecycleBin,
}

/// Насколько безопасно чистить пункт. Уровни идут от самого безопасного к самому
/// рискованному — на этом порядке построены встроенные наборы галочек.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Safety {
    /// Временные файлы и кэши: всё пересоздаётся само, ничего нужного не теряется.
    Safe,
    /// Безопасно, но с последствиями: что-то придётся скачать заново
    /// или пропадут данные для диагностики сбоев.
    Moderate,
    /// Удаляются данные, которые могут понадобиться: журналы событий, история Защитника,
    /// файлы для восстановления установленных программ.
    Caution,
}

impl Safety {
    pub const ALL: [Safety; 3] = [Safety::Safe, Safety::Moderate, Safety::Caution];

    /// Название уровня на языке интерфейса: «Безопасно», «Умеренно», «Осторожно».
    pub fn name(self) -> String {
        match self {
            Safety::Safe => t!("safety.safe"),
            Safety::Moderate => t!("safety.moderate"),
            Safety::Caution => t!("safety.caution"),
        }
    }

    /// Чем грозит очистка пунктов этого уровня.
    pub fn hint(self) -> String {
        match self {
            Safety::Safe => t!("safety.safe.hint"),
            Safety::Moderate => t!("safety.moderate.hint"),
            Safety::Caution => t!("safety.caution.hint"),
        }
    }

    /// Отмечать ли пункт, если пользователь его ещё ни разу не трогал.
    pub fn selected_by_default(self) -> bool {
        self <= Safety::Moderate
    }
}

/// Программа, которую можно закрыть перед очисткой.
pub struct Program {
    /// Главный исполняемый файл программы.
    pub exe: &'static str,
    /// Вспомогательные процессы программы. Если файлы держат они, закрывается вся программа.
    pub helpers: &'static [&'static str],
}

impl Program {
    /// Главный файл и все вспомогательные.
    pub fn all_exes(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.exe).chain(self.helpers.iter().copied())
    }
}

/// Браузеры. Chromium, Cent Browser и Chrome запускаются одним и тем же `chrome.exe`.
const BROWSERS: &[Program] = &[
    Program { exe: "chrome.exe", helpers: &[] },
    Program { exe: "msedge.exe", helpers: &[] },
    Program { exe: "firefox.exe", helpers: &[] },
    Program { exe: "opera.exe", helpers: &[] },
    Program { exe: "browser.exe", helpers: &[] },
    Program { exe: "brave.exe", helpers: &[] },
    Program { exe: "vivaldi.exe", helpers: &[] },
];

/// Лаунчеры игр.
const GAME_LAUNCHERS: &[Program] = &[
    Program { exe: "steam.exe", helpers: &["steamwebhelper.exe"] },
    Program { exe: "EpicGamesLauncher.exe", helpers: &["EpicWebHelper.exe"] },
];

/// Встроенные категории в том порядке, в котором они показываются. Свои — после них.
pub const BUILT_IN_CATEGORIES: [&str; 5] = ["temp", "caches", "windows", "reports", CUSTOM_CATEGORY];

/// Категория своих пунктов, у которых она не указана.
pub const CUSTOM_CATEGORY: &str = "custom";

/// Категория пункта: группа в списке окна и в `HexCleanerCli list`.
#[derive(Clone, Debug, PartialEq)]
pub enum Category {
    /// Встроенная категория (одна из [`BUILT_IN_CATEGORIES`]); название — в переводах.
    BuiltIn(&'static str),
    /// Своя категория из файла своего пункта: её название.
    Custom(LocalizedText),
}

impl Category {
    pub fn name(&self) -> String {
        match self {
            Category::BuiltIn("temp") => t!("category.temp"),
            Category::BuiltIn("caches") => t!("category.caches"),
            Category::BuiltIn("windows") => t!("category.windows"),
            Category::BuiltIn("reports") => t!("category.reports"),
            Category::BuiltIn(_) => t!("category.custom"),
            Category::Custom(name) => name.resolve().unwrap_or_default(),
        }
    }

    /// Ключ категории, не зависящий от языка: по нему пункты группируются
    /// и запоминается, свёрнута ли категория.
    pub fn key(&self) -> String {
        match self {
            Category::BuiltIn(id) => (*id).to_owned(),
            Category::Custom(LocalizedText::Text(name)) => format!("custom:{name}"),
            Category::Custom(LocalizedText::ByLanguage(names)) => {
                let name = names.get("en").or_else(|| names.values().next()).map_or("", String::as_str);
                format!("custom:{name}")
            }
        }
    }
}

/// Пункты одной категории.
pub struct CategoryGroup {
    pub category: Category,
    /// Индексы пунктов в каталоге, в порядке каталога.
    pub tasks: Vec<usize>,
}

/// Пункты по категориям: встроенные категории в своём порядке, затем свои — в порядке,
/// в котором они встретились. Пустые категории не попадают в список.
pub fn group_by_category(tasks: &[CleanupTask]) -> Vec<CategoryGroup> {
    let mut groups: Vec<CategoryGroup> = BUILT_IN_CATEGORIES
        .iter()
        .map(|&id| CategoryGroup { category: Category::BuiltIn(id), tasks: Vec::new() })
        .collect();
    for (index, task) in tasks.iter().enumerate() {
        let key = task.category.key();
        match groups.iter_mut().find(|group| group.category.key() == key) {
            Some(group) => group.tasks.push(index),
            None => groups.push(CategoryGroup { category: task.category.clone(), tasks: vec![index] }),
        }
    }
    groups.retain(|group| !group.tasks.is_empty());
    groups
}

/// Пункт очистки, который пользователь видит в списке.
pub struct CleanupTask {
    /// Стабильный идентификатор — по нему сохраняется выбор пользователя
    /// и находятся заголовок и описание в файлах перевода.
    pub id: &'static str,
    pub category: Category,
    pub safety: Safety,
    /// Программы, которые перед очисткой можно предложить закрыть, если они держат
    /// файлы пункта (браузеры — свой кэш, Steam — свой). Только явный список:
    /// общие папки вроде кэша шейдеров видеокарты или `INetCache` держат десятки
    /// посторонних программ и даже компоненты Windows — их закрывать нельзя.
    pub programs_to_close: &'static [Program],
    pub requires_admin: bool,
    /// Службы, которые останавливаются на время очистки и затем запускаются снова.
    pub services_to_stop: &'static [&'static str],
    /// Свой пункт из JSON-файла; `None` — встроенный.
    pub custom: Option<CustomText>,
    pub actions: Vec<Action>,
}

/// Тексты своего пункта и файл, из которого он взят.
pub struct CustomText {
    pub title: LocalizedText,
    pub description: Option<LocalizedText>,
    /// Имя файла в папке своих пунктов, например `gradle.json`.
    pub file: String,
}

impl CleanupTask {
    pub fn title(&self) -> String {
        match &self.custom {
            Some(custom) => custom.title.resolve().unwrap_or_else(|| self.id.to_owned()),
            None => i18n::text(&format!("task.{}.title", self.id)),
        }
    }

    /// Подробности, которые показываются во всплывающей подсказке.
    pub fn description(&self) -> String {
        match &self.custom {
            Some(custom) => {
                let description = custom.description.as_ref().and_then(LocalizedText::resolve);
                let source = t!("custom_task.source", file = custom.file);
                description.map_or(source.clone(), |description| format!("{description}\n\n{source}"))
            }
            None => i18n::text(&format!("task.{}.description", self.id)),
        }
    }

    /// Удаляет ли пункт файлы. Только такие можно переносить в корзину: журналы событий,
    /// память и корзина Windows очищаются средствами системы.
    pub fn deletes_files(&self) -> bool {
        self.actions.iter().any(|action| {
            matches!(action, Action::ClearDirectory { .. } | Action::DeleteDirectory { .. } | Action::DeleteFiles { .. })
        })
    }
}

fn clear_dir(pattern: impl Into<String>) -> Action {
    Action::ClearDirectory { pattern: pattern.into(), min_age: None }
}

fn clear_dir_older_than(pattern: impl Into<String>, min_age: Duration) -> Action {
    Action::ClearDirectory { pattern: pattern.into(), min_age: Some(min_age) }
}

fn delete_dir(pattern: impl Into<String>) -> Action {
    Action::DeleteDirectory { pattern: pattern.into() }
}

fn delete_files(pattern: impl Into<String>) -> Action {
    Action::DeleteFiles { pattern: pattern.into() }
}

/// Все пункты очистки — встроенные, затем свои — в том порядке, в котором они показываются
/// и выполняются. Свои пункты с ошибками пропускаются; узнать о них — [`load_tasks`].
pub fn all_tasks() -> Vec<CleanupTask> {
    load_tasks().0
}

/// Все пункты очистки и ошибки в файлах своих пунктов.
pub fn load_tasks() -> (Vec<CleanupTask>, Vec<String>) {
    let mut tasks = built_in_tasks();
    let ids: Vec<&str> = tasks.iter().map(|task| task.id).collect();
    let (custom, errors) = custom_tasks::load(&ids);
    tasks.extend(custom);
    (tasks, errors)
}

/// Встроенные пункты очистки.
pub fn built_in_tasks() -> Vec<CleanupTask> {
    vec![
        CleanupTask {
            id: "memory",
            category: Category::BuiltIn("temp"),
            safety: Safety::Moderate,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &[],
            custom: None,
            actions: vec![Action::TrimProcessMemory],
        },
        CleanupTask {
            id: "games_gpu_cache",
            category: Category::BuiltIn("caches"),
            safety: Safety::Safe,
            programs_to_close: GAME_LAUNCHERS,
            requires_admin: false,
            services_to_stop: &[],
            custom: None,
            actions: games_and_gpu_cache_actions(),
        },
        CleanupTask {
            id: "installers_cache",
            category: Category::BuiltIn("caches"),
            safety: Safety::Caution,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &[],
            custom: None,
            actions: vec![
                clear_dir(r"%PROGRAMDATA%\NVIDIA Corporation\Installer2"),
                clear_dir(r"%PROGRAMDATA%\NVIDIA Corporation\Downloader"),
                delete_dir(r"%SYSTEMDRIVE%\MSOCache"),
            ],
        },
        CleanupTask {
            id: "browsers_cache",
            category: Category::BuiltIn("caches"),
            safety: Safety::Safe,
            programs_to_close: BROWSERS,
            requires_admin: false,
            services_to_stop: &[],
            custom: None,
            actions: browser_cache_actions(),
        },
        CleanupTask {
            id: "explorer_cache",
            category: Category::BuiltIn("caches"),
            safety: Safety::Safe,
            programs_to_close: &[],
            requires_admin: false,
            services_to_stop: &[],
            custom: None,
            actions: vec![
                delete_files(r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db"),
                delete_files(r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\iconcache_*.db"),
                delete_files(r"%LOCALAPPDATA%\IconCache.db"),
            ],
        },
        CleanupTask {
            id: "defender_cache",
            category: Category::BuiltIn("windows"),
            safety: Safety::Caution,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &[],
            custom: None,
            actions: vec![
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows Defender\Scans\History\Results"),
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows Defender\Scans\History\Service\DetectionHistory"),
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows Defender\Support"),
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows Defender\Network Inspection System\Support"),
            ],
        },
        CleanupTask {
            id: "windows_setup_cache",
            category: Category::BuiltIn("windows"),
            safety: Safety::Caution,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &[],
            custom: None,
            actions: vec![
                clear_dir(r"%WINDIR%\Installer\$PatchCache$"),
                delete_dir(r"%SYSTEMDRIVE%\$Windows.~WS"),
                delete_dir(r"%SYSTEMDRIVE%\$GetCurrent"),
                delete_dir(r"%SYSTEMDRIVE%\ESD"),
            ],
        },
        CleanupTask {
            id: "windows_update_cache",
            category: Category::BuiltIn("windows"),
            safety: Safety::Moderate,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &["wuauserv", "bits", "dosvc"],
            custom: None,
            actions: vec![
                clear_dir(r"%WINDIR%\SoftwareDistribution\Download"),
                clear_dir(
                    r"%WINDIR%\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache",
                ),
            ],
        },
        CleanupTask {
            id: "system_logs",
            category: Category::BuiltIn("windows"),
            safety: Safety::Caution,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &[],
            custom: None,
            actions: system_log_actions(),
        },
        CleanupTask {
            id: "driver_leftovers",
            category: Category::BuiltIn("windows"),
            safety: Safety::Safe,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &[],
            custom: None,
            actions: vec![
                delete_dir(r"%SYSTEMDRIVE%\NVIDIA"),
                delete_dir(r"%SYSTEMDRIVE%\AMD"),
                clear_dir(r"%SYSTEMDRIVE%\Intel\Logs"),
            ],
        },
        CleanupTask {
            id: "user_temp",
            category: Category::BuiltIn("temp"),
            safety: Safety::Safe,
            programs_to_close: &[],
            requires_admin: false,
            services_to_stop: &[],
            custom: None,
            actions: vec![clear_dir_older_than(r"%TEMP%", TEMP_MIN_AGE)],
        },
        CleanupTask {
            id: "system_temp",
            category: Category::BuiltIn("temp"),
            safety: Safety::Safe,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &[],
            custom: None,
            actions: vec![
                clear_dir_older_than(r"%WINDIR%\Temp", TEMP_MIN_AGE),
                clear_dir_older_than(r"%WINDIR%\SystemTemp", TEMP_MIN_AGE),
                clear_dir_older_than(r"%WINDIR%\ServiceProfiles\LocalService\AppData\Local\Temp", TEMP_MIN_AGE),
                clear_dir_older_than(r"%WINDIR%\ServiceProfiles\NetworkService\AppData\Local\Temp", TEMP_MIN_AGE),
            ],
        },
        CleanupTask {
            id: "diagnostics",
            category: Category::BuiltIn("reports"),
            safety: Safety::Moderate,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &[],
            custom: None,
            actions: vec![
                clear_dir(r"%PROGRAMDATA%\Microsoft\Diagnosis\ETLLogs"),
                clear_dir(r"%WINDIR%\System32\SleepStudy"),
                clear_dir(r"%WINDIR%\System32\WDI\LogFiles"),
            ],
        },
        CleanupTask {
            id: "user_reports",
            category: Category::BuiltIn("reports"),
            safety: Safety::Safe,
            programs_to_close: &[],
            requires_admin: false,
            services_to_stop: &[],
            custom: None,
            actions: vec![
                clear_dir(r"%LOCALAPPDATA%\Microsoft\Windows\WER"),
                clear_dir(r"%LOCALAPPDATA%\CrashDumps"),
            ],
        },
        CleanupTask {
            id: "system_reports",
            category: Category::BuiltIn("reports"),
            safety: Safety::Moderate,
            programs_to_close: &[],
            requires_admin: true,
            services_to_stop: &[],
            custom: None,
            actions: vec![
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows\WER\ReportArchive"),
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows\WER\ReportQueue"),
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows\WER\Temp"),
                clear_dir(r"%WINDIR%\Minidump"),
                clear_dir(r"%WINDIR%\LiveKernelReports"),
                delete_files(r"%WINDIR%\MEMORY.DMP"),
            ],
        },
        CleanupTask {
            id: "recycle_bin",
            category: Category::BuiltIn("temp"),
            // Это файлы, которые пользователь удалил сам: после очистки их не вернуть.
            safety: Safety::Caution,
            programs_to_close: &[],
            requires_admin: false,
            services_to_stop: &[],
            custom: None,
            actions: vec![Action::EmptyRecycleBin],
        },
    ]
}

/// Папки с пользовательскими данными браузеров на движке Chromium.
/// Внутри каждой лежат профили (`Default`, `Profile 1` ...).
const CHROMIUM_BROWSERS: &[&str] = &[
    r"%LOCALAPPDATA%\Google\Chrome\User Data",
    r"%LOCALAPPDATA%\Microsoft\Edge\User Data",
    r"%LOCALAPPDATA%\Yandex\YandexBrowser\User Data",
    r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data",
    r"%LOCALAPPDATA%\Vivaldi\User Data",
    r"%LOCALAPPDATA%\CentBrowser\User Data",
    r"%LOCALAPPDATA%\Chromium\User Data",
    r"%LOCALAPPDATA%\Opera Software\Opera Stable",
    r"%LOCALAPPDATA%\Opera Software\Opera GX Stable",
];

/// Папки кэша внутри профиля Chromium.
const CHROMIUM_PROFILE_CACHES: &[&str] =
    &["Cache", "Code Cache", "GPUCache", "DawnCache", "DawnGraphiteCache", "DawnWebGPUCache"];

/// Папки кэша, общие для всех профилей Chromium.
const CHROMIUM_SHARED_CACHES: &[&str] = &["ShaderCache", "GrShaderCache", "GraphiteDawnCache"];

/// Папки кэша внутри профиля Firefox (локальная часть профиля, без пользовательских данных).
const FIREFOX_PROFILE_CACHES: &[&str] = &["cache2", "startupCache", "thumbnails", "jumpListCache"];

fn browser_cache_actions() -> Vec<Action> {
    let mut actions = Vec::new();

    for browser in CHROMIUM_BROWSERS {
        for cache in CHROMIUM_SHARED_CACHES {
            actions.push(clear_dir(format!(r"{browser}\{cache}")));
        }
        for cache in CHROMIUM_PROFILE_CACHES {
            // Opera хранит кэш прямо в папке браузера, остальные — в папке профиля.
            actions.push(clear_dir(format!(r"{browser}\{cache}")));
            actions.push(clear_dir(format!(r"{browser}\*\{cache}")));
        }
    }

    for cache in FIREFOX_PROFILE_CACHES {
        actions.push(clear_dir(format!(r"%LOCALAPPDATA%\Mozilla\Firefox\Profiles\*\{cache}")));
    }

    // Кэш движка Internet Explorer / WebView, которым пользуются многие программы.
    actions.push(clear_dir(r"%LOCALAPPDATA%\Microsoft\Windows\INetCache"));
    actions
}

fn games_and_gpu_cache_actions() -> Vec<Action> {
    vec![
        // DirectX
        clear_dir(r"%LOCALAPPDATA%\D3DSCache"),
        // NVIDIA
        clear_dir(r"%LOCALAPPDATA%\NVIDIA\DXCache"),
        clear_dir(r"%LOCALAPPDATA%\NVIDIA\GLCache"),
        clear_dir(r"%LOCALAPPDATA%\NVIDIA\OptixCache"),
        clear_dir(r"%LOCALAPPDATA%\NVIDIA Corporation\NV_Cache"),
        clear_dir(r"%APPDATA%\NVIDIA\ComputeCache"),
        clear_dir(r"%USERPROFILE%\AppData\LocalLow\NVIDIA\PerDriverVersion\DXCache"),
        // AMD
        clear_dir(r"%LOCALAPPDATA%\AMD\DxCache"),
        clear_dir(r"%LOCALAPPDATA%\AMD\DxcCache"),
        clear_dir(r"%LOCALAPPDATA%\AMD\GLCache"),
        clear_dir(r"%LOCALAPPDATA%\AMD\VkCache"),
        // Intel
        clear_dir(r"%LOCALAPPDATA%\Intel\ShaderCache"),
        clear_dir(r"%USERPROFILE%\AppData\LocalLow\Intel\ShaderCache"),
        // Steam
        clear_dir(r"%STEAM%\appcache\httpcache"),
        clear_dir(r"%STEAM%\steamapps\shadercache"),
        clear_dir(r"%STEAM%\logs"),
        clear_dir(r"%STEAM%\dumps"),
        clear_dir(r"%LOCALAPPDATA%\Steam\htmlcache"),
        // Epic Games Launcher
        clear_dir(r"%LOCALAPPDATA%\EpicGamesLauncher\Saved\webcache*"),
        clear_dir(r"%LOCALAPPDATA%\EpicGamesLauncher\Saved\Logs"),
    ]
}

fn system_log_actions() -> Vec<Action> {
    let mut actions = vec![Action::ClearEventLogs];

    // Подпапки %WINDIR%\Logs, в которых лежат только журналы обслуживания системы.
    for folder in ["CBS", "DISM", "DPX", "MoSetup", "NetSetup", "SIH", "WindowsUpdate", "waasmedic"] {
        actions.push(clear_dir(format!(r"%WINDIR%\Logs\{folder}")));
    }

    actions.push(delete_files(r"%WINDIR%\*.log"));
    actions.push(delete_files(r"%WINDIR%\Panther\*.log"));
    actions.push(delete_files(r"%WINDIR%\INF\setupapi*.log"));
    actions.push(delete_files(r"%WINDIR%\debug\*.log"));
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    /// У каждого встроенного пункта — встроенная категория; группы идут в порядке
    /// [`BUILT_IN_CATEGORIES`], внутри — в порядке каталога, пустых нет.
    #[test]
    fn built_in_tasks_are_grouped_by_category() {
        let tasks = built_in_tasks();
        assert!(tasks.iter().all(|task| matches!(task.category, Category::BuiltIn(id) if BUILT_IN_CATEGORIES.contains(&id))));

        let groups = group_by_category(&tasks);
        let keys: Vec<String> = groups.iter().map(|group| group.category.key()).collect();
        assert_eq!(keys, ["temp", "caches", "windows", "reports"]);
        assert_eq!(groups.iter().map(|group| group.tasks.len()).sum::<usize>(), tasks.len());
        assert!(groups.iter().all(|group| group.tasks.windows(2).all(|pair| pair[0] < pair[1])));
    }

    #[test]
    fn custom_categories_follow_built_in_ones() {
        let mut tasks = built_in_tasks();
        let custom = |id: &'static str, category: Category| CleanupTask {
            id,
            category,
            safety: Safety::Safe,
            programs_to_close: &[],
            requires_admin: false,
            services_to_stop: &[],
            custom: None,
            actions: Vec::new(),
        };
        let development = || {
            Category::Custom(LocalizedText::ByLanguage(
                [("ru".to_owned(), "Разработка".to_owned()), ("en".to_owned(), "Development".to_owned())].into(),
            ))
        };
        tasks.push(custom("gradle", development()));
        tasks.push(custom("claude", Category::BuiltIn("caches")));
        tasks.push(custom("npm", development()));
        tasks.push(custom("mine", Category::BuiltIn(CUSTOM_CATEGORY)));

        let groups = group_by_category(&tasks);
        let keys: Vec<String> = groups.iter().map(|group| group.category.key()).collect();
        assert_eq!(keys, ["temp", "caches", "windows", "reports", "custom", "custom:Development"]);
        let ids = |key: &str| -> Vec<&str> {
            let group = groups.iter().find(|group| group.category.key() == key).unwrap();
            group.tasks.iter().map(|&index| tasks[index].id).collect()
        };
        assert_eq!(ids("custom:Development"), ["gradle", "npm"]);
        assert_eq!(ids("caches").last(), Some(&"claude"));
        assert_eq!(ids("custom"), ["mine"]);
    }
}
