//! Каталог пунктов очистки.
//!
//! Каждый пункт описывается декларативно: идентификатор, нужны ли права администратора
//! и список простых действий ([`Action`]). Заголовок и описание пункта берутся из файлов
//! перевода по ключам `task.<id>.title` и `task.<id>.description`. Движок очистки (`cleaner`) и интерфейс
//! (`app`) ничего не знают о конкретных путях — чтобы добавить новый пункт,
//! достаточно дописать его в [`all_tasks`].
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

use crate::i18n;
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

/// Пункт очистки, который пользователь видит в списке.
pub struct CleanupTask {
    /// Стабильный идентификатор — по нему сохраняется выбор пользователя
    /// и находятся заголовок и описание в файлах перевода.
    pub id: &'static str,
    pub safety: Safety,
    pub requires_admin: bool,
    /// Службы, которые останавливаются на время очистки и затем запускаются снова.
    pub services_to_stop: &'static [&'static str],
    pub actions: Vec<Action>,
}

impl CleanupTask {
    pub fn title(&self) -> String {
        i18n::text(&format!("task.{}.title", self.id))
    }

    /// Подробности, которые показываются во всплывающей подсказке.
    pub fn description(&self) -> String {
        i18n::text(&format!("task.{}.description", self.id))
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

/// Все пункты очистки в том порядке, в котором они показываются и выполняются.
pub fn all_tasks() -> Vec<CleanupTask> {
    vec![
        CleanupTask {
            id: "memory",
            safety: Safety::Moderate,
            requires_admin: true,
            services_to_stop: &[],
            actions: vec![Action::TrimProcessMemory],
        },
        CleanupTask {
            id: "games_gpu_cache",
            safety: Safety::Safe,
            requires_admin: false,
            services_to_stop: &[],
            actions: games_and_gpu_cache_actions(),
        },
        CleanupTask {
            id: "installers_cache",
            safety: Safety::Caution,
            requires_admin: true,
            services_to_stop: &[],
            actions: vec![
                clear_dir(r"%PROGRAMDATA%\NVIDIA Corporation\Installer2"),
                clear_dir(r"%PROGRAMDATA%\NVIDIA Corporation\Downloader"),
                delete_dir(r"%SYSTEMDRIVE%\MSOCache"),
            ],
        },
        CleanupTask {
            id: "browsers_cache",
            safety: Safety::Safe,
            requires_admin: false,
            services_to_stop: &[],
            actions: browser_cache_actions(),
        },
        CleanupTask {
            id: "explorer_cache",
            safety: Safety::Safe,
            requires_admin: false,
            services_to_stop: &[],
            actions: vec![
                delete_files(r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db"),
                delete_files(r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\iconcache_*.db"),
                delete_files(r"%LOCALAPPDATA%\IconCache.db"),
            ],
        },
        CleanupTask {
            id: "defender_cache",
            safety: Safety::Caution,
            requires_admin: true,
            services_to_stop: &[],
            actions: vec![
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows Defender\Scans\History\Results"),
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows Defender\Scans\History\Service\DetectionHistory"),
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows Defender\Support"),
                clear_dir(r"%PROGRAMDATA%\Microsoft\Windows Defender\Network Inspection System\Support"),
            ],
        },
        CleanupTask {
            id: "windows_setup_cache",
            safety: Safety::Caution,
            requires_admin: true,
            services_to_stop: &[],
            actions: vec![
                clear_dir(r"%WINDIR%\Installer\$PatchCache$"),
                delete_dir(r"%SYSTEMDRIVE%\$Windows.~WS"),
                delete_dir(r"%SYSTEMDRIVE%\$GetCurrent"),
                delete_dir(r"%SYSTEMDRIVE%\ESD"),
            ],
        },
        CleanupTask {
            id: "windows_update_cache",
            safety: Safety::Moderate,
            requires_admin: true,
            services_to_stop: &["wuauserv", "bits", "dosvc"],
            actions: vec![
                clear_dir(r"%WINDIR%\SoftwareDistribution\Download"),
                clear_dir(
                    r"%WINDIR%\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache",
                ),
            ],
        },
        CleanupTask {
            id: "system_logs",
            safety: Safety::Caution,
            requires_admin: true,
            services_to_stop: &[],
            actions: system_log_actions(),
        },
        CleanupTask {
            id: "driver_leftovers",
            safety: Safety::Safe,
            requires_admin: true,
            services_to_stop: &[],
            actions: vec![
                delete_dir(r"%SYSTEMDRIVE%\NVIDIA"),
                delete_dir(r"%SYSTEMDRIVE%\AMD"),
                clear_dir(r"%SYSTEMDRIVE%\Intel\Logs"),
            ],
        },
        CleanupTask {
            id: "user_temp",
            safety: Safety::Safe,
            requires_admin: false,
            services_to_stop: &[],
            actions: vec![clear_dir_older_than(r"%TEMP%", TEMP_MIN_AGE)],
        },
        CleanupTask {
            id: "system_temp",
            safety: Safety::Safe,
            requires_admin: true,
            services_to_stop: &[],
            actions: vec![
                clear_dir_older_than(r"%WINDIR%\Temp", TEMP_MIN_AGE),
                clear_dir_older_than(r"%WINDIR%\SystemTemp", TEMP_MIN_AGE),
                clear_dir_older_than(r"%WINDIR%\ServiceProfiles\LocalService\AppData\Local\Temp", TEMP_MIN_AGE),
                clear_dir_older_than(r"%WINDIR%\ServiceProfiles\NetworkService\AppData\Local\Temp", TEMP_MIN_AGE),
            ],
        },
        CleanupTask {
            id: "diagnostics",
            safety: Safety::Moderate,
            requires_admin: true,
            services_to_stop: &[],
            actions: vec![
                clear_dir(r"%PROGRAMDATA%\Microsoft\Diagnosis\ETLLogs"),
                clear_dir(r"%WINDIR%\System32\SleepStudy"),
                clear_dir(r"%WINDIR%\System32\WDI\LogFiles"),
            ],
        },
        CleanupTask {
            id: "user_reports",
            safety: Safety::Safe,
            requires_admin: false,
            services_to_stop: &[],
            actions: vec![
                clear_dir(r"%LOCALAPPDATA%\Microsoft\Windows\WER"),
                clear_dir(r"%LOCALAPPDATA%\CrashDumps"),
            ],
        },
        CleanupTask {
            id: "system_reports",
            safety: Safety::Moderate,
            requires_admin: true,
            services_to_stop: &[],
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
            // Это файлы, которые пользователь удалил сам: после очистки их не вернуть.
            safety: Safety::Caution,
            requires_admin: false,
            services_to_stop: &[],
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
