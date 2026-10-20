//! Обновление программы из релизов GitHub.
//!
//! Последний релиз берётся из GitHub API (черновики и предварительные выпуски туда не
//! попадают). Его тег — номер версии, как в `Cargo.toml` (`0.2.0`, `v0.2.0`, `0.2.0-beta.1`);
//! к релизу приложен zip-архив с `HexCleaner.exe` и `HexCleanerCli.exe`. Архив сверяется
//! с контрольной суммой SHA-256, которую GitHub считает сам. При запуске GitHub спрашиваем
//! не чаще раза в [`CHECK_INTERVAL_MINUTES`] минут; вручную — когда угодно.
//!
//! Работающий exe Windows не даёт перезаписать, но даёт переименовать. Поэтому новые файлы
//! сначала распаковываются рядом (`*.update-new`), затем старые переименовываются
//! (`*.update-old`), а новые встают на их место. Старые файлы удаляются при следующем
//! запуске ([`remove_leftovers`]). Путь к программе не меняется — задания Планировщика
//! продолжают работать.

use std::cmp::Ordering;
use std::fmt;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Days, Local, TimeDelta};
use serde::{Deserialize, Serialize};

use crate::system::{hash, http};
use crate::t;

/// Версия этой сборки.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Страница релизов — для ссылки в настройках.
pub const RELEASES_PAGE: &str = "https://github.com/ModdingTechNik/HexCleaner/releases";

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/ModdingTechNik/HexCleaner/releases/latest";

/// Как часто можно проверять обновления при запуске. Кнопка «Проверить сейчас» не ограничена.
pub const CHECK_INTERVAL_MINUTES: i64 = 5;

/// Имя главной программы в архиве релиза: им заменяется запущенный exe, как бы он ни назывался.
const MAIN_EXE: &str = "HexCleaner.exe";

/// Окончания имён временных файлов обновления.
const NEW_SUFFIX: &str = ".update-new";
const OLD_SUFFIX: &str = ".update-old";

/// Настройки обновлений (часть [`crate::settings::Settings`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Проверять ли обновления при запуске окна.
    pub check_on_start: bool,
    /// До какого времени не проверять при запуске («Напомнить через …»).
    pub postponed_until: Option<DateTime<Local>>,
    /// Версия, которую пользователь решил пропустить: при запуске о ней не напоминаем.
    pub skipped_version: Option<String>,
    /// Когда GitHub в последний раз ответил на проверку.
    pub last_check: Option<DateTime<Local>>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { check_on_start: true, postponed_until: None, skipped_version: None, last_check: None }
    }
}

/// Ответ на предложение обновиться, кроме «Обновить».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Postpone {
    /// Напомнить через столько дней.
    Days(u64),
    /// Не напоминать об этой версии; о следующей — напомнить.
    SkipVersion,
    /// Не проверять обновления при запуске (включается снова в настройках).
    DisableChecks,
}

impl UpdateSettings {
    /// Проверять ли обновления при этом запуске.
    pub fn should_check_on_start(&self, now: DateTime<Local>) -> bool {
        self.check_on_start && self.postponed_until.is_none_or(|until| now >= until) && self.next_check(now).is_none()
    }

    /// Когда можно будет проверить снова; `None` — уже можно.
    /// Если часы перевели назад и последняя проверка оказалась «в будущем», проверять можно.
    pub fn next_check(&self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        let last = self.last_check.filter(|last| *last <= now)?;
        let next = last + TimeDelta::minutes(CHECK_INTERVAL_MINUTES);
        (now < next).then_some(next)
    }

    /// Предлагать ли версию, найденную при запуске. Проверка вручную предлагает любую.
    pub fn should_offer(&self, release: &Release) -> bool {
        self.skipped_version.as_deref() != Some(release.version.as_str())
    }

    pub fn postpone(&mut self, choice: Postpone, release: &Release, now: DateTime<Local>) {
        match choice {
            Postpone::Days(days) => self.postponed_until = now.checked_add_days(Days::new(days)),
            Postpone::SkipVersion => self.skipped_version = Some(release.version.clone()),
            Postpone::DisableChecks => self.check_on_start = false,
        }
    }
}

/// Релиз на GitHub.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Номер версии — тег без `v`.
    pub version: String,
    /// Описание релиза («что нового»).
    pub notes: String,
    pub page_url: String,
    pub download_url: String,
    /// Размер архива, байт.
    pub size: u64,
    /// SHA-256 архива (шестнадцатеричный), если GitHub его сообщил.
    pub sha256: Option<String>,
}

/// Почему не удалось проверить или установить обновление.
#[derive(Debug)]
pub enum UpdateError {
    Network(http::HttpError),
    /// GitHub ответил не тем, что ожидалось.
    InvalidResponse,
    /// Тег последнего релиза — не номер версии.
    BadTag(String),
    /// В релизе этой версии нет zip-архива.
    NoArchive(String),
    ChecksumMismatch,
    /// Архив повреждён.
    BadArchive(String),
    /// В архиве нет `HexCleaner.exe`.
    NoProgramInArchive,
    /// Нет прав на запись в папку программы.
    AccessDenied(PathBuf),
    Install(io::Error),
    Cancelled,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self {
            UpdateError::Network(error) => t!("update.error.network", error = error),
            UpdateError::InvalidResponse => t!("update.error.invalid_response"),
            UpdateError::BadTag(tag) => t!("update.error.bad_tag", tag = tag),
            UpdateError::NoArchive(version) => t!("update.error.no_archive", version = version),
            UpdateError::ChecksumMismatch => t!("update.error.checksum"),
            UpdateError::BadArchive(error) => t!("update.error.bad_archive", error = error),
            UpdateError::NoProgramInArchive => t!("update.error.no_program", exe = MAIN_EXE),
            UpdateError::AccessDenied(dir) => t!("update.error.access_denied", dir = dir.display()),
            UpdateError::Install(error) => t!("update.error.install", error = error),
            UpdateError::Cancelled => t!("update.error.cancelled"),
        };
        f.write_str(&text)
    }
}

impl From<http::HttpError> for UpdateError {
    fn from(error: http::HttpError) -> Self {
        match error {
            http::HttpError::Cancelled => UpdateError::Cancelled,
            error => UpdateError::Network(error),
        }
    }
}

/// Итог проверки обновлений.
#[derive(Clone, Debug, PartialEq)]
pub enum CheckResult {
    /// Вышла версия новее этой.
    Available(Release),
    /// Установлена последняя версия.
    UpToDate,
    /// Эта сборка новее последнего релиза: например, собрана до его выпуска
    /// или это предварительная версия, а последний релиз — более старый.
    Ahead { latest: String },
    /// Опубликованных релизов нет: только черновики или предварительные выпуски,
    /// которые программа не предлагает.
    NoReleases,
}

/// Спрашивает у GitHub последний релиз и сравнивает его с этой сборкой.
pub fn check() -> Result<CheckResult, UpdateError> {
    let headers = ["Accept: application/vnd.github+json", "X-GitHub-Api-Version: 2022-11-28"];
    let body = match http::get(LATEST_RELEASE_API, &user_agent(), &headers, |_, _| true) {
        Ok(body) => body,
        // Так GitHub отвечает, когда у репозитория нет ни одного опубликованного релиза.
        Err(http::HttpError::Status(404)) => return Ok(CheckResult::NoReleases),
        Err(error) => return Err(error.into()),
    };
    let json = String::from_utf8(body).map_err(|_| UpdateError::InvalidResponse)?;
    classify(&json, CURRENT_VERSION)
}

/// Сравнивает последний релиз (ответ GitHub `json`) с версией `current`.
fn classify(json: &str, current: &str) -> Result<CheckResult, UpdateError> {
    let release: GitHubRelease = serde_json::from_str(json).map_err(|_| UpdateError::InvalidResponse)?;
    let latest = Version::parse(&release.tag_name).ok_or_else(|| UpdateError::BadTag(release.tag_name.clone()))?;
    let current = Version::parse(current).ok_or_else(|| UpdateError::BadTag(current.to_owned()))?;
    let version = display_version(&release.tag_name);
    match latest.cmp(&current) {
        Ordering::Less => Ok(CheckResult::Ahead { latest: version }),
        Ordering::Equal => Ok(CheckResult::UpToDate),
        // Архив нужен, только если версия новее: у старых релизов его может и не быть.
        Ordering::Greater => Ok(CheckResult::Available(release.into_release(version)?)),
    }
}

/// Загружает архив релиза и ставит его файлы на место программы `exe` (путь к запущенному exe).
/// `on_progress(загружено, всего)`; вернёт `false` — загрузка остановится.
/// После успеха программу нужно перезапустить по тому же пути `exe`.
pub fn download_and_install(
    release: &Release,
    exe: &Path,
    on_progress: impl FnMut(u64, Option<u64>) -> bool,
) -> Result<(), UpdateError> {
    let dir = exe.parent().ok_or_else(|| UpdateError::AccessDenied(exe.to_owned()))?;
    // Права проверяем до загрузки, чтобы не качать архив зря.
    check_writable(dir)?;

    let archive = http::get(&release.download_url, &user_agent(), &[], on_progress)?;
    if let Some(expected) = &release.sha256 {
        let actual = hash::sha256_hex(&archive).ok_or(UpdateError::ChecksumMismatch)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(UpdateError::ChecksumMismatch);
        }
    }
    install_archive(&archive, exe)
}

/// Удаляет файлы, оставшиеся от прошлого обновления, из папки программы `exe`.
pub fn remove_leftovers(exe: &Path) {
    let Some(dir) = exe.parent() else { return };
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(OLD_SUFFIX) || name.ends_with(NEW_SUFFIX) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn user_agent() -> String {
    format!("HexCleaner/{CURRENT_VERSION}")
}

/// Тег для показа: `" v0.2.0 "` → `"0.2.0"`.
fn display_version(tag: &str) -> String {
    tag.trim().trim_start_matches(['v', 'V']).to_owned()
}

/// Номер версии по правилам SemVer, но с любым числом частей: `0.10.0` новее `0.9.1`,
/// `0.1` и `0.1.0.0` — одна версия, предварительная `0.2.0-beta.2` старше `0.2.0`,
/// но новее `0.2.0-beta.1` и `0.1.9`. Метаданные сборки (`+…`) не учитываются.
#[derive(Debug, PartialEq, Eq)]
struct Version {
    /// Числовые части без нулей в конце.
    numbers: Vec<u64>,
    /// Части после `-`; пусто — окончательная версия.
    pre_release: Vec<PreRelease>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PreRelease {
    // Порядок вариантов важен: по SemVer числовые части младше буквенных.
    Number(u64),
    Text(String),
}

impl Version {
    fn parse(text: &str) -> Option<Self> {
        let text = display_version(text);
        let text = text.split_once('+').map_or(text.as_str(), |(version, _)| version);
        let (core, pre_release) = match text.split_once('-') {
            Some((core, pre_release)) => (core, Some(pre_release)),
            None => (text, None),
        };

        let mut numbers = core.split('.').map(|part| part.parse().ok()).collect::<Option<Vec<u64>>>()?;
        while numbers.last() == Some(&0) {
            numbers.pop();
        }
        let pre_release = match pre_release {
            None => Vec::new(),
            Some(pre_release) => pre_release
                .split('.')
                .map(|part| match part.parse() {
                    Ok(number) => Some(PreRelease::Number(number)),
                    Err(_) if !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => {
                        Some(PreRelease::Text(part.to_ascii_lowercase()))
                    }
                    Err(_) => None,
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Self { numbers, pre_release })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers.cmp(&other.numbers).then_with(|| match (self.pre_release.is_empty(), other.pre_release.is_empty()) {
            (true, true) => Ordering::Equal,
            // Окончательная версия новее предварительной с тем же номером.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => self.pre_release.cmp(&other.pre_release),
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Ответ `GET /repos/{owner}/{repo}/releases/latest` (только нужные поля).
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// `"sha256:…"`; у файлов, загруженных до середины 2025 года, его нет.
    #[serde(default)]
    digest: Option<String>,
}

impl GitHubRelease {
    /// Релиз версии `version` с его zip-архивом.
    fn into_release(self, version: String) -> Result<Release, UpdateError> {
        let asset = self
            .assets
            .into_iter()
            .find(|asset| asset.name.to_ascii_lowercase().ends_with(".zip"))
            .ok_or_else(|| UpdateError::NoArchive(version.clone()))?;
        Ok(Release {
            version,
            notes: self.body.unwrap_or_default().trim().to_owned(),
            page_url: self.html_url,
            download_url: asset.browser_download_url,
            size: asset.size,
            sha256: asset.digest.and_then(|digest| digest.strip_prefix("sha256:").map(str::to_owned)),
        })
    }
}

/// Можно ли создавать файлы в папке программы.
fn check_writable(dir: &Path) -> Result<(), UpdateError> {
    let probe = dir.join(format!("write-test{NEW_SUFFIX}"));
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Err(UpdateError::AccessDenied(dir.to_owned())),
        Err(error) => Err(UpdateError::Install(error)),
    }
}

/// Распаковывает архив на место программы `exe`: `HexCleaner.exe` из архива заменяет `exe`,
/// остальные файлы ложатся рядом с ним под своими именами (папки в архиве не учитываются).
fn install_archive(archive: &[u8], exe: &Path) -> Result<(), UpdateError> {
    let dir = exe.parent().ok_or_else(|| UpdateError::AccessDenied(exe.to_owned()))?;
    let files = read_archive(archive)?;
    if !files.iter().any(|(name, _)| name.eq_ignore_ascii_case(MAIN_EXE)) {
        return Err(UpdateError::NoProgramInArchive);
    }
    let targets: Vec<PathBuf> = files
        .iter()
        .map(|(name, _)| if name.eq_ignore_ascii_case(MAIN_EXE) { exe.to_owned() } else { dir.join(name) })
        .collect();

    // Сначала все новые файлы рядом: если не хватит места, программа останется как была.
    for (target, (_, data)) in targets.iter().zip(&files) {
        if let Err(error) = fs::write(with_suffix(target, NEW_SUFFIX), data) {
            remove_all(&targets, NEW_SUFFIX);
            return Err(UpdateError::Install(error));
        }
    }

    // Затем подмена: старый файл в сторону, новый на его место. При ошибке — откат.
    let mut replaced: Vec<(&PathBuf, bool)> = Vec::new();
    for target in &targets {
        match replace(target) {
            Ok(had_old) => replaced.push((target, had_old)),
            Err(error) => {
                for (target, had_old) in replaced.into_iter().rev() {
                    let _ = fs::remove_file(target);
                    if had_old {
                        let _ = fs::rename(with_suffix(target, OLD_SUFFIX), target);
                    }
                }
                remove_all(&targets, NEW_SUFFIX);
                return Err(UpdateError::Install(error));
            }
        }
    }
    Ok(())
}

/// Файлы архива: имя (без папок) и содержимое. Контрольные суммы файлов zip проверяет при чтении.
fn read_archive(archive: &[u8]) -> Result<Vec<(String, Vec<u8>)>, UpdateError> {
    let bad = |error: &dyn fmt::Display| UpdateError::BadArchive(error.to_string());
    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).map_err(|error| bad(&error))?;
    let mut files = Vec::new();
    for index in 0..zip.len() {
        let mut file = zip.by_index(index).map_err(|error| bad(&error))?;
        if file.is_dir() {
            continue;
        }
        let Some(name) = file.enclosed_name().and_then(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
        else {
            continue;
        };
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(|error| bad(&error))?;
        files.push((name, data));
    }
    Ok(files)
}

/// Ставит `target.update-new` на место `target`; старый файл, если был, становится `target.update-old`.
fn replace(target: &Path) -> io::Result<bool> {
    let old = with_suffix(target, OLD_SUFFIX);
    let had_old = target.exists();
    if had_old {
        // Остаток прошлого обновления, если его не удалось удалить при запуске.
        let _ = fs::remove_file(&old);
        fs::rename(target, &old)?;
    }
    if let Err(error) = fs::rename(with_suffix(target, NEW_SUFFIX), target) {
        if had_old {
            let _ = fs::rename(&old, target);
        }
        return Err(error);
    }
    Ok(had_old)
}

fn remove_all(targets: &[PathBuf], suffix: &str) {
    for target in targets {
        let _ = fs::remove_file(with_suffix(target, suffix));
    }
}

/// `C:\a\b.exe` + `.update-new` → `C:\a\b.exe.update-new`.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use chrono::TimeZone;
    use zip::write::SimpleFileOptions;

    use super::*;

    const RELEASE_JSON: &str = r#"{
        "tag_name": "v0.2.0",
        "html_url": "https://github.com/ModdingTechNik/HexCleaner/releases/tag/v0.2.0",
        "body": "Что нового\r\n",
        "assets": [
            {"name": "notes.txt", "browser_download_url": "https://x/notes.txt", "size": 1},
            {"name": "release.zip", "browser_download_url": "https://x/release.zip", "size": 6097965,
             "digest": "sha256:578fa6c9df3af1c0ffaf8462a807cd8f1486de91c3a95e5a479f2d1ca7c74046"}
        ]
    }"#;

    fn release() -> Release {
        match classify(RELEASE_JSON, "0.1.0").unwrap() {
            CheckResult::Available(release) => release,
            other => panic!("{other:?}"),
        }
    }

    fn with_tag(tag: &str) -> String {
        RELEASE_JSON.replace("v0.2.0", tag)
    }

    /// `a` новее `b`.
    fn newer(a: &str, b: &str) -> bool {
        Version::parse(a).unwrap() > Version::parse(b).unwrap()
    }

    fn same(a: &str, b: &str) -> bool {
        Version::parse(a).unwrap() == Version::parse(b).unwrap()
    }

    #[test]
    fn compares_versions_by_parts() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("v0.10.0", "0.9.1"));
        assert!(newer("1.0.0.1", "1.0.0"));
        assert!(same("0.1.0.0", "0.1.0"));
        assert!(same("V0.1", "0.1.0+build.5"));
        assert!(newer("0.1.0", "0.0.9"));
    }

    #[test]
    fn pre_releases_are_older_than_the_final_version() {
        assert!(newer("0.2.0", "0.2.0-beta"));
        assert!(newer("0.2.0-beta", "0.1.0"));
        assert!(newer("0.2.0-beta", "0.2.0-alpha"));
        assert!(newer("0.2.0-beta.10", "0.2.0-beta.2"));
        assert!(newer("0.2.0-beta.1", "0.2.0-beta"));
        assert!(newer("0.2.0-rc", "0.2.0-beta.5"));
        assert!(same("0.2.0-Beta.1", "0.2.0-beta.1"));
        for invalid in ["", "latest", "release-1", "1.x", "0.2.0-", "0.2.0-beta..1"] {
            assert_eq!(Version::parse(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn classifies_latest_release() {
        let release = release();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.notes, "Что нового");
        assert_eq!(release.download_url, "https://x/release.zip");
        assert_eq!(release.size, 6097965);
        assert_eq!(release.sha256.as_deref(), Some("578fa6c9df3af1c0ffaf8462a807cd8f1486de91c3a95e5a479f2d1ca7c74046"));

        assert_eq!(classify(RELEASE_JSON, "0.2.0").unwrap(), CheckResult::UpToDate);
        // Своя сборка новее релиза.
        assert_eq!(classify(RELEASE_JSON, "0.3.0").unwrap(), CheckResult::Ahead { latest: "0.2.0".into() });
        // Предварительная версия 0.2.0 обновляется до окончательной, а 0.3.0-beta — нет.
        assert!(matches!(classify(RELEASE_JSON, "0.2.0-beta.1").unwrap(), CheckResult::Available(_)));
        assert_eq!(classify(RELEASE_JSON, "0.3.0-beta").unwrap(), CheckResult::Ahead { latest: "0.2.0".into() });

        assert!(matches!(classify(&with_tag("latest"), "0.1.0"), Err(UpdateError::BadTag(tag)) if tag == "latest"));
        assert!(matches!(classify("{}", "0.1.0"), Err(UpdateError::InvalidResponse)));
        // Архив нужен только у версии новее установленной.
        let without_zip = RELEASE_JSON.replace("release.zip", "release.7z");
        assert!(matches!(classify(&without_zip, "0.1.0"), Err(UpdateError::NoArchive(version)) if version == "0.2.0"));
        assert_eq!(classify(&without_zip, "0.2.0").unwrap(), CheckResult::UpToDate);
    }

    #[test]
    fn checks_at_most_every_five_minutes() {
        let now = Local.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
        let mut settings = UpdateSettings { last_check: Some(now), ..UpdateSettings::default() };
        let later = |minutes| now + TimeDelta::minutes(minutes);

        assert_eq!(settings.next_check(later(4)), Some(later(5)));
        assert!(!settings.should_check_on_start(later(4)));
        assert_eq!(settings.next_check(later(5)), None);
        assert!(settings.should_check_on_start(later(5)));
        // Часы перевели назад — не ждём.
        assert_eq!(settings.next_check(later(-60)), None);

        settings.last_check = None;
        assert_eq!(settings.next_check(now), None);
    }

    #[test]
    fn postponing_and_skipping() {
        let now = Local.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
        let release = release();
        let mut settings = UpdateSettings::default();
        assert!(settings.should_check_on_start(now));

        settings.postpone(Postpone::Days(3), &release, now);
        assert!(!settings.should_check_on_start(now.checked_add_days(Days::new(2)).unwrap()));
        assert!(settings.should_check_on_start(now.checked_add_days(Days::new(3)).unwrap()));

        assert!(settings.should_offer(&release));
        settings.postpone(Postpone::SkipVersion, &release, now);
        assert!(!settings.should_offer(&release));
        assert!(settings.should_offer(&Release { version: "0.3.0".into(), ..release.clone() }));

        settings.postpone(Postpone::DisableChecks, &release, now);
        assert!(!settings.should_check_on_start(now.checked_add_days(Days::new(30)).unwrap()));
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hexcleaner-update-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn installs_archive_next_to_program_and_cleans_up() {
        let dir = temp_dir("install");
        // Программу могли переименовать — главный exe из архива всё равно встаёт на её место.
        let exe = dir.join("Cleaner.exe");
        fs::write(&exe, b"old gui").unwrap();
        fs::write(dir.join("HexCleanerCli.exe"), b"old cli").unwrap();

        let archive = zip_of(&[("HexCleaner.exe", b"new gui"), ("bin/HexCleanerCli.exe", b"new cli")]);
        install_archive(&archive, &exe).unwrap();

        assert_eq!(fs::read(&exe).unwrap(), b"new gui");
        assert_eq!(fs::read(dir.join("HexCleanerCli.exe")).unwrap(), b"new cli");
        assert_eq!(fs::read(dir.join("Cleaner.exe.update-old")).unwrap(), b"old gui");
        assert!(!dir.join("HexCleaner.exe").exists());

        remove_leftovers(&exe);
        let mut names: Vec<String> =
            fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into()).collect();
        names.sort();
        assert_eq!(names, ["Cleaner.exe", "HexCleanerCli.exe"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_archive_without_program() {
        let dir = temp_dir("reject");
        let exe = dir.join("HexCleaner.exe");
        fs::write(&exe, b"old").unwrap();

        let archive = zip_of(&[("readme.txt", b"hi")]);
        assert!(matches!(install_archive(&archive, &exe), Err(UpdateError::NoProgramInArchive)));
        assert!(matches!(install_archive(b"not a zip", &exe), Err(UpdateError::BadArchive(_))));
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Обращается к GitHub: `cargo test -- --ignored latest_release_is_reachable`.
    #[test]
    #[ignore]
    fn latest_release_is_reachable() {
        let headers = ["Accept: application/vnd.github+json"];
        let body = http::get(LATEST_RELEASE_API, &user_agent(), &headers, |_, _| true).unwrap();
        // Сравниваем с версией 0.0.1, чтобы последний релиз точно оказался новее.
        let release = match classify(&String::from_utf8(body).unwrap(), "0.0.1").unwrap() {
            CheckResult::Available(release) => release,
            other => panic!("{other:?}"),
        };
        let archive = http::get(&release.download_url, &user_agent(), &[], |_, _| true).unwrap();
        assert_eq!(archive.len() as u64, release.size);
        assert_eq!(hash::sha256_hex(&archive), release.sha256);
        assert!(read_archive(&archive).unwrap().iter().any(|(name, _)| name == MAIN_EXE));
    }
}
