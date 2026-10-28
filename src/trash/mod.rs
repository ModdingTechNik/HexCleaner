//! Корзина HexCleaner: удалённое при очистке можно вернуть.
//!
//! Каждая очистка, перенёсшая что-то в корзину, получает свою папку `Trash\2026-10-28 14-03-12\`.
//! Внутри повторяется полный путь файла, а вместо диска — папка с его буквой:
//! `C:\Windows\Temp\a.tmp` → `Trash\<очистка>\c\Windows\Temp\a.tmp`. Рядом лежит описание
//! очистки `<очистка>.json`: какие файлы каким пунктом перенесены.
//!
//! Сжатие: NTFS — файлы папки очистки сжимаются файловой системой (как галочка «Сжимать
//! содержимое» в свойствах папки), ZIP — папка очистки упаковывается в `<очистка>.zip`
//! с той же структурой внутри.
//!
//! Лимит считается по размеру того, что лежит в корзине: у архивов — по размеру архива.

mod remove;
mod session;

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf, Prefix};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub use remove::{MANUAL_TASK_ID, RemoveReport, remove_path};
pub use session::{LimitRequest, Stored, TrashDecision, TrashSession};

use crate::system::delete;
use crate::t;

/// Лимит корзины по умолчанию.
pub const DEFAULT_LIMIT: u64 = 5 * 1024 * 1024 * 1024;

/// Что делать с файлами пункта при очистке.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrashPolicy {
    /// В корзину, если не превышен её лимит.
    #[default]
    Normal,
    /// Всегда в корзину, не глядя на лимит.
    Always,
    /// Удалять сразу, мимо корзины.
    Never,
}

pub use crate::settings::Compression;

/// Настройки корзины (часть [`crate::settings::Settings`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrashSettings {
    /// Лимит, байт: `Some(0)` — корзина отключена, `None` — без ограничений.
    pub limit: Option<u64>,
    pub compress: bool,
    /// NTFS — сжимаются файлы папки очистки, ZIP — папка очистки упаковывается в архив.
    pub compression: Compression,
    /// Сколько дней хранить очистки; более старые удаляются при запуске программы
    /// и перед очисткой. `None` — хранить, пока не вытеснит лимит.
    pub keep_days: Option<u32>,
    /// Своя папка корзины; `None` — `%LOCALAPPDATA%\HexCleaner\Trash`.
    pub path: Option<PathBuf>,
    /// Что делать с файлами пунктов (ключ — id пункта); пунктов без записи — [`TrashPolicy::Normal`].
    pub policies: BTreeMap<String, TrashPolicy>,
}

impl Default for TrashSettings {
    fn default() -> Self {
        Self {
            limit: Some(DEFAULT_LIMIT),
            compress: false,
            compression: Compression::Ntfs,
            keep_days: None,
            path: None,
            policies: BTreeMap::new(),
        }
    }
}

impl TrashSettings {
    pub fn is_enabled(&self) -> bool {
        self.limit != Some(0)
    }

    pub fn directory(&self) -> PathBuf {
        self.path.clone().unwrap_or_else(default_directory)
    }

    pub fn policy(&self, task_id: &str) -> TrashPolicy {
        self.policies.get(task_id).copied().unwrap_or_default()
    }

    pub fn set_policy(&mut self, task_id: &str, policy: TrashPolicy) {
        if policy == TrashPolicy::default() {
            self.policies.remove(task_id);
        } else {
            self.policies.insert(task_id.to_owned(), policy);
        }
    }
}

pub fn default_directory() -> PathBuf {
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_owned());
    Path::new(&local_app_data).join("HexCleaner").join("Trash")
}

/// Одна очистка в корзине.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrashRun {
    /// Имя папки очистки: `2026-10-28 14-03-12`.
    pub id: String,
    pub created: DateTime<Local>,
    pub items: Vec<TrashItem>,
    /// Очистка упакована в zip-архив.
    #[serde(skip)]
    pub archived: bool,
    /// Сколько места очистка занимает в корзине, байт.
    #[serde(skip)]
    pub stored_bytes: u64,
}

/// Файлы одного пункта очистки.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrashItem {
    /// Id пункта; пусто, если описание очистки потерялось и файлы найдены по папке.
    pub task: String,
    /// Название пункта на языке, который был включён при очистке.
    pub title: String,
    pub files: Vec<TrashFile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrashFile {
    /// Где файл лежал до очистки.
    pub path: PathBuf,
    pub size: u64,
}

impl TrashRun {
    pub fn file_count(&self) -> usize {
        self.items.iter().map(|item| item.files.len()).sum()
    }

    pub fn total_size(&self) -> u64 {
        self.items.iter().map(TrashItem::total_size).sum()
    }
}

impl TrashItem {
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

/// Итог восстановления.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RestoreReport {
    pub restored: u64,
    pub restored_bytes: u64,
    /// Файлы не восстановлены, потому что на их месте уже есть новые (остались в корзине).
    pub existing: u64,
    /// Файлы, которые не удалось восстановить, и причины.
    pub failed: Vec<(PathBuf, String)>,
}

/// Папка корзины.
pub struct Trash {
    dir: PathBuf,
}

impl Trash {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn run_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    fn archive_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.zip"))
    }

    fn manifest_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Очистки в корзине, новые первыми.
    pub fn runs(&self) -> Vec<TrashRun> {
        let Ok(entries) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
                match name.rsplit_once('.') {
                    _ if is_dir => Some(name),
                    Some((id, "json" | "zip")) => Some(id.to_owned()),
                    _ => None,
                }
            })
            .collect();
        ids.sort();
        ids.dedup();
        let mut runs: Vec<TrashRun> = ids.iter().filter_map(|id| self.load(id)).collect();
        runs.sort_by(|a, b| b.id.cmp(&a.id));
        runs
    }

    /// Очистка по имени; без описания — по содержимому папки или архива.
    pub fn load(&self, id: &str) -> Option<TrashRun> {
        let archive = self.archive_path(id);
        let archived = archive.is_file();
        let mut run = match fs::read(self.manifest_path(id)).ok().and_then(|data| serde_json::from_slice(&data).ok()) {
            Some(run) => run,
            None => self.reconstruct(id, archived)?,
        };
        run.id = id.to_owned();
        run.archived = archived;
        run.stored_bytes = if archived { fs::metadata(&archive).map_or(0, |metadata| metadata.len()) } else { run.total_size() };
        if run.items.iter().all(|item| item.files.is_empty()) {
            return None;
        }
        Some(run)
    }

    /// Описание очистки потерялось (например, программу закрыли посреди очистки):
    /// восстанавливаем список файлов по папке или архиву. Пункты неизвестны.
    fn reconstruct(&self, id: &str, archived: bool) -> Option<TrashRun> {
        let mut files = Vec::new();
        if archived {
            let mut zip = ZipArchive::new(File::open(self.archive_path(id)).ok()?).ok()?;
            for index in 0..zip.len() {
                let Ok(entry) = zip.by_index(index) else { continue };
                if let Some(path) = entry.enclosed_name().and_then(|name| original_path(&name)) {
                    files.push(TrashFile { path, size: entry.size() });
                }
            }
        } else {
            let root = self.run_dir(id);
            walk_files(&root, &mut |path, size| {
                if let Some(original) = path.strip_prefix(&root).ok().and_then(original_path) {
                    files.push(TrashFile { path: original, size });
                }
            });
        }
        let created = fs::metadata(self.run_dir(id)).or_else(|_| fs::metadata(self.archive_path(id))).ok()?.modified().ok()?;
        let item = TrashItem { task: String::new(), title: t!("trash.unknown_item"), files };
        Some(TrashRun { id: id.to_owned(), created: created.into(), items: vec![item], archived, stored_bytes: 0 })
    }

    /// Сколько места занимает корзина, байт.
    pub fn used_bytes(&self) -> u64 {
        self.runs().iter().map(|run| run.stored_bytes).sum()
    }

    /// Возвращает на место файлы очистки `run_id`, для которых `filter` вернёт `true`.
    /// Файлы, на месте которых уже есть новые, не трогаются и остаются в корзине.
    pub fn restore(&self, run_id: &str, filter: impl Fn(&TrashItem, &TrashFile) -> bool) -> RestoreReport {
        let mut report = RestoreReport::default();
        let Some(mut run) = self.load(run_id) else { return report };
        let mut zip = if run.archived {
            match File::open(self.archive_path(run_id)).map_err(io::Error::other).and_then(|file| ZipArchive::new(file).map_err(io::Error::other)) {
                Ok(zip) => Some(zip),
                Err(error) => {
                    report.failed.push((self.archive_path(run_id), error.to_string()));
                    return report;
                }
            }
        } else {
            None
        };

        let mut restored_entries = Vec::new();
        for item in &mut run.items {
            let mut kept = Vec::with_capacity(item.files.len());
            for file in std::mem::take(&mut item.files) {
                if !filter(item, &file) {
                    kept.push(file);
                    continue;
                }
                let Some(relative) = mirror_path(&file.path) else {
                    kept.push(file);
                    continue;
                };
                if file.path.exists() {
                    report.existing += 1;
                    kept.push(file);
                    continue;
                }
                if let Some(parent) = file.path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let result = match &mut zip {
                    Some(zip) => extract(zip, &relative, &file.path),
                    None => delete::move_file(&self.run_dir(run_id).join(&relative), &file.path).map_err(|error| error.to_string()),
                };
                match result {
                    Ok(()) => {
                        report.restored += 1;
                        report.restored_bytes += file.size;
                        restored_entries.push(zip_name(&relative));
                    }
                    Err(error) => {
                        report.failed.push((file.path.clone(), error));
                        kept.push(file);
                    }
                }
            }
            item.files = kept;
        }
        run.items.retain(|item| !item.files.is_empty());
        drop(zip);

        if run.items.is_empty() {
            let _ = self.delete_run(run_id);
            return report;
        }
        if run.archived && !restored_entries.is_empty() {
            let _ = self.remove_from_archive(run_id, &restored_entries);
        } else if !run.archived {
            remove_empty_dirs(&self.run_dir(run_id));
        }
        let _ = self.save(&run);
        report
    }

    /// Удаляет очистку из корзины навсегда.
    pub fn delete_run(&self, run_id: &str) -> io::Result<()> {
        let mut result = Ok(());
        let dir = self.run_dir(run_id);
        if dir.exists() {
            clear_readonly_tree(&dir);
            result = fs::remove_dir_all(&dir);
        }
        for path in [self.archive_path(run_id), self.manifest_path(run_id)] {
            match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => result = Err(error),
                _ => {}
            }
        }
        result
    }

    /// Удаляет очистки старше `days` дней. Возвращает, сколько удалено.
    pub fn remove_expired(&self, days: u32, now: DateTime<Local>) -> usize {
        let Some(oldest_kept) = now.checked_sub_days(chrono::Days::new(u64::from(days))) else { return 0 };
        self.runs().iter().filter(|run| run.created < oldest_kept && self.delete_run(&run.id).is_ok()).count()
    }

    /// Удаляет из корзины всё.
    pub fn empty(&self) -> io::Result<()> {
        let mut result = Ok(());
        for run in self.runs() {
            if let Err(error) = self.delete_run(&run.id) {
                result = Err(error);
            }
        }
        result
    }

    /// Удаляет самые старые очистки (кроме `keep`), пока в лимит `limit` не поместится ещё
    /// `needed` байт. Возвращает `true`, если место нашлось.
    pub fn free_space(&self, needed: u64, limit: u64, keep: Option<&str>) -> bool {
        let mut runs = self.runs();
        let mut used: u64 = runs.iter().filter(|run| Some(run.id.as_str()) != keep).map(|run| run.stored_bytes).sum();
        // Самые старые — в конце списка.
        while used + needed > limit {
            let Some(oldest) = runs.pop() else { break };
            if Some(oldest.id.as_str()) == keep {
                continue;
            }
            if self.delete_run(&oldest.id).is_ok() {
                used = used.saturating_sub(oldest.stored_bytes);
            }
        }
        used + needed <= limit
    }

    fn save(&self, run: &TrashRun) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let data = serde_json::to_vec(run).map_err(io::Error::other)?;
        fs::write(self.manifest_path(&run.id), data)
    }

    /// Упаковывает папку очистки в zip-архив и удаляет папку.
    fn pack(&self, run_id: &str) -> io::Result<()> {
        let dir = self.run_dir(run_id);
        let archive = self.archive_path(run_id);
        let result = (|| {
            let mut zip = ZipWriter::new(File::create(&archive)?);
            let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).large_file(true);
            let mut error = None;
            walk_files(&dir, &mut |path, _| {
                if error.is_some() {
                    return;
                }
                let Ok(relative) = path.strip_prefix(&dir) else { return };
                let result = zip
                    .start_file(zip_name(relative), options)
                    .map_err(io::Error::other)
                    .and_then(|()| io::copy(&mut File::open(path)?, &mut zip).map(|_| ()));
                error = result.err();
            });
            if let Some(error) = error {
                return Err(error);
            }
            zip.finish().map_err(io::Error::other)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                clear_readonly_tree(&dir);
                let _ = fs::remove_dir_all(&dir);
                Ok(())
            }
            Err(error) => {
                // Недописанный архив не нужен: файлы остаются в папке.
                let _ = fs::remove_file(&archive);
                Err(error)
            }
        }
    }

    /// Переписывает архив без восстановленных файлов.
    fn remove_from_archive(&self, run_id: &str, names: &[String]) -> io::Result<()> {
        let archive_path = self.archive_path(run_id);
        let temp_path = self.dir.join(format!("{run_id}.zip.tmp"));
        let mut source = ZipArchive::new(File::open(&archive_path)?).map_err(io::Error::other)?;
        let mut target = ZipWriter::new(File::create(&temp_path)?);
        for index in 0..source.len() {
            let entry = source.by_index_raw(index).map_err(io::Error::other)?;
            let entry_name = entry.name().map(|name| name.into_owned()).unwrap_or_default();
            if !names.iter().any(|name| name.eq_ignore_ascii_case(&entry_name)) {
                target.raw_copy_file(entry).map_err(io::Error::other)?;
            }
        }
        target.finish().map_err(io::Error::other)?;
        drop(source);
        fs::rename(&temp_path, &archive_path)
    }
}

/// `C:\Windows\a.tmp` → `c\Windows\a.tmp`; сетевой путь `\\server\share\a` → `unc\server\share\a`.
/// `None` — путь не абсолютный или с `..`.
pub fn mirror_path(path: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    let mut mirror = match components.next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                PathBuf::from((letter as char).to_ascii_lowercase().to_string())
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                Path::new("unc").join(server).join(share)
            }
            _ => return None,
        },
        _ => return None,
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => mirror.push(part),
            _ => return None,
        }
    }
    Some(mirror)
}

/// Обратно к [`mirror_path`]: `c\Windows\a.tmp` → `C:\Windows\a.tmp`.
pub fn original_path(mirror: &Path) -> Option<PathBuf> {
    let mut parts = mirror.components().map(|component| match component {
        Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
        _ => None,
    });
    let first = parts.next()??;
    let mut path = if first.eq_ignore_ascii_case("unc") {
        let (server, share) = (parts.next()??, parts.next()??);
        PathBuf::from(format!(r"\\{server}\{share}\"))
    } else if first.len() == 1 && first.chars().all(|c| c.is_ascii_alphabetic()) {
        PathBuf::from(format!(r"{}:\", first.to_ascii_uppercase()))
    } else {
        return None;
    };
    for part in parts {
        path.push(part?);
    }
    Some(path)
}

/// Имя файла в zip-архиве: части пути через `/`.
fn zip_name(relative: &Path) -> String {
    relative.components().map(|component| component.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

fn extract(zip: &mut ZipArchive<File>, relative: &Path, target: &Path) -> Result<(), String> {
    let mut entry = zip.by_name(&zip_name(relative)).map_err(|error| error.to_string())?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data).map_err(|error| error.to_string())?;
    fs::write(target, data).map_err(|error| error.to_string())
}

/// Все файлы внутри папки (без перехода по ссылкам): путь и размер.
fn walk_files(dir: &Path, visit: &mut dyn FnMut(&Path, u64)) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if crate::cleaner::is_link(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            walk_files(&entry.path(), visit);
        } else {
            visit(&entry.path(), metadata.len());
        }
    }
}

/// Удаляет пустые папки внутри `dir` и саму `dir`, если она опустела.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                remove_empty_dirs(&entry.path());
            }
        }
    }
    let _ = fs::remove_dir(dir);
}

/// Снимает «только чтение» со всех файлов, иначе папку не удалить.
fn clear_readonly_tree(dir: &Path) {
    walk_files(dir, &mut |path, _| {
        if let Ok(metadata) = fs::metadata(path) {
            let mut permissions = metadata.permissions();
            if permissions.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = fs::set_permissions(path, permissions);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Папка для теста: в ней «диск» с мусором и корзина.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hexcleaner-trash-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn settings(trash: &Path, limit: Option<u64>) -> TrashSettings {
        TrashSettings { limit, path: Some(trash.to_owned()), ..TrashSettings::default() }
    }

    fn junk(dir: &Path, name: &str, size: usize) -> PathBuf {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![b'x'; size]).unwrap();
        path
    }

    fn never_asked(_: LimitRequest) -> TrashDecision {
        panic!("лимит не должен был превыситься")
    }

    #[test]
    fn mirror_paths_round_trip() {
        let path = Path::new(r"D:\WinTemp\filetmp1.dmp");
        let mirror = mirror_path(path).unwrap();
        assert_eq!(mirror, Path::new(r"d\WinTemp\filetmp1.dmp"));
        assert_eq!(original_path(&mirror).unwrap(), path);

        let unc = Path::new(r"\\server\share\cache\a.bin");
        assert_eq!(mirror_path(unc).unwrap(), Path::new(r"unc\server\share\cache\a.bin"));
        assert_eq!(original_path(&mirror_path(unc).unwrap()).unwrap(), unc);

        assert_eq!(mirror_path(Path::new(r"relative\a.tmp")), None);
        assert_eq!(mirror_path(Path::new(r"C:\a\..\b")), None);
        assert_eq!(original_path(Path::new(r"notadrive\a")), None);
    }

    #[test]
    fn moves_files_and_restores_them_selectively() {
        let root = temp_dir("restore");
        let trash_dir = root.join("Trash");
        let a = junk(&root, r"cache\a.tmp", 100);
        let b = junk(&root, r"cache\sub\b.tmp", 200);
        let c = junk(&root, r"logs\c.log", 300);

        let mut session = TrashSession::new(&settings(&trash_dir, None)).unwrap();
        for path in [&a, &b] {
            assert!(matches!(session.store("user_temp", "Temp", path, 0, &mut never_asked), Stored::Moved));
        }
        assert!(matches!(session.store("system_logs", "Logs", &c, 0, &mut never_asked), Stored::Moved));
        let id = session.finish().unwrap();
        assert!(!a.exists() && !b.exists() && !c.exists());

        // Внутри папки очистки — полный путь с буквой диска вместо «C:».
        let mirrored = trash_dir.join(&id).join(mirror_path(&b).unwrap());
        assert!(mirrored.is_file(), "{}", mirrored.display());

        let trash = Trash::new(trash_dir.clone());
        let runs = trash.runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].items.len(), 2);
        assert_eq!(runs[0].file_count(), 3);

        // Папка: восстанавливается всё, что внутри неё, в том числе во вложенных папках.
        let folder = root.join("cache");
        let report = trash.restore(&id, |_, file| file.path.starts_with(&folder));
        assert_eq!((report.restored, report.existing), (2, 0));
        assert!(a.exists() && b.exists() && !c.exists());

        // На месте файла уже новый — он остаётся в корзине, новый не затирается.
        fs::write(&c, "new").unwrap();
        let report = trash.restore(&id, |item, _| item.task == "system_logs");
        assert_eq!((report.restored, report.existing), (0, 1));
        assert_eq!(fs::read_to_string(&c).unwrap(), "new");

        fs::remove_file(&c).unwrap();
        let report = trash.restore(&id, |_, _| true);
        assert_eq!(report.restored, 1);
        assert_eq!(fs::read(&c).unwrap().len(), 300);
        // Всё восстановлено — очистка исчезла из корзины.
        assert!(trash.runs().is_empty());
        assert!(!trash_dir.join(&id).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_runs_restore_and_shrink() {
        let root = temp_dir("zip");
        let trash_dir = root.join("Trash");
        let a = junk(&root, r"cache\a.tmp", 5000);
        let b = junk(&root, r"cache\b.tmp", 5000);

        let zip_settings = TrashSettings { compress: true, compression: Compression::Zip, ..settings(&trash_dir, None) };
        let mut session = TrashSession::new(&zip_settings).unwrap();
        session.store("user_temp", "Temp", &a, 5000, &mut never_asked);
        session.store("user_temp", "Temp", &b, 5000, &mut never_asked);
        let id = session.finish().unwrap();

        let trash = Trash::new(trash_dir.clone());
        assert!(!trash_dir.join(&id).exists(), "папка должна быть упакована");
        let run = trash.load(&id).unwrap();
        assert!(run.archived);
        assert!(run.stored_bytes < 10_000, "архив не сжат: {}", run.stored_bytes);

        let report = trash.restore(&id, |_, file| file.path == a);
        assert_eq!(report.restored, 1);
        assert_eq!(fs::read(&a).unwrap(), vec![b'x'; 5000]);
        // В архиве остался только второй файл.
        let mut zip = ZipArchive::new(File::open(trash.archive_path(&id)).unwrap()).unwrap();
        assert_eq!(zip.len(), 1);
        assert!(zip.by_name(&zip_name(&mirror_path(&b).unwrap())).is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn limit_policies_and_decisions() {
        let root = temp_dir("limit");
        let trash_dir = root.join("Trash");

        // Старая очистка занимает 600 из 1000 байт.
        let mut old = TrashSession::new(&settings(&trash_dir, None)).unwrap();
        old.store("user_temp", "Temp", &junk(&root, "old.tmp", 600), 600, &mut never_asked);
        let old_id = old.finish().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let mut limited = settings(&trash_dir, Some(1000));
        limited.set_policy("always", TrashPolicy::Always);
        limited.set_policy("never", TrashPolicy::Never);
        let mut session = TrashSession::new(&limited).unwrap();

        // Помещается — без вопросов.
        assert!(matches!(session.store("user_temp", "Temp", &junk(&root, "a.tmp", 300), 300, &mut never_asked), Stored::Moved));
        // «Удалять сразу» — мимо корзины.
        assert!(matches!(session.store("never", "N", &junk(&root, "n.tmp", 10), 10, &mut never_asked), Stored::NotMoved));
        // «Всегда в корзину» — лимит не важен.
        assert!(matches!(session.store("always", "A", &junk(&root, "big.tmp", 5000), 5000, &mut never_asked), Stored::Moved));

        // Лимит превышен: спрашивают один раз, ответ действует до конца очистки.
        let mut asked = 0;
        let mut answer = |request: LimitRequest| {
            asked += 1;
            assert_eq!((request.used, request.limit), (600, 1000));
            TrashDecision::FreeSpace
        };
        // Освобождение места удаляет старую очистку, но 5300 байт в 1000 всё равно не влезают.
        assert!(matches!(session.store("user_temp", "Temp", &junk(&root, "b.tmp", 300), 300, &mut answer), Stored::NotMoved));
        assert!(matches!(session.store("user_temp", "Temp", &junk(&root, "c.tmp", 300), 300, &mut answer), Stored::NotMoved));
        assert_eq!(asked, 1);
        let id = session.finish().unwrap();

        let trash = Trash::new(trash_dir.clone());
        assert!(trash.load(&old_id).is_none(), "старая очистка должна быть удалена");
        let run = trash.load(&id).unwrap();
        assert_eq!(run.file_count(), 2);

        // «Остановить» возвращается как есть.
        let mut session = TrashSession::new(&settings(&trash_dir, Some(1))).unwrap();
        let stop = &mut |_| TrashDecision::Stop;
        assert!(matches!(session.store("user_temp", "Temp", &junk(&root, "d.tmp", 10), 10, stop), Stored::Stopped));
        assert_eq!(session.finish(), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn removes_folder_to_trash_or_permanently() {
        let root = temp_dir("remove");
        let trash_dir = root.join("Trash");
        let folder = root.join("big");
        junk(&folder, "a.bin", 100);
        junk(&folder.join("sub"), "b.bin", 50);

        let report = remove_path(&folder, "Disk", Some(&settings(&trash_dir, None)), TrashDecision::IgnoreLimit);
        assert_eq!((report.files, report.bytes), (2, 150));
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(!folder.exists());
        let trash = Trash::new(trash_dir.clone());
        let run = trash.load(report.trash_run.as_deref().unwrap()).unwrap();
        assert_eq!(run.items[0].task, MANUAL_TASK_ID);
        assert_eq!(trash.restore(&run.id, |_, _| true).restored, 2);
        assert!(folder.join("sub").join("b.bin").exists());

        // Без корзины — навсегда.
        let report = remove_path(&folder, "Disk", None, TrashDecision::IgnoreLimit);
        assert_eq!((report.files, report.trash_run), (2, None));
        assert!(!folder.exists());
        assert!(trash.runs().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expired_cleanups_are_removed() {
        let root = temp_dir("expired");
        let trash_dir = root.join("Trash");
        let mut session = TrashSession::new(&settings(&trash_dir, None)).unwrap();
        session.store("user_temp", "Temp", &junk(&root, "a.tmp", 10), 10, &mut never_asked);
        let id = session.finish().unwrap();

        let trash = Trash::new(trash_dir.clone());
        let created = trash.load(&id).unwrap().created;
        assert_eq!(trash.remove_expired(7, created + chrono::Days::new(6)), 0);
        assert_eq!(trash.remove_expired(7, created + chrono::Days::new(8)), 1);
        assert!(trash.runs().is_empty());

        // Срок задан в настройках — старые очистки удаляются, когда начинается новая.
        let mut session = TrashSession::new(&settings(&trash_dir, None)).unwrap();
        session.store("user_temp", "Temp", &junk(&root, "b.tmp", 10), 10, &mut never_asked);
        let old = session.finish().unwrap();
        let mut manifest: TrashRun = serde_json::from_slice(&fs::read(trash.manifest_path(&old)).unwrap()).unwrap();
        manifest.created -= chrono::TimeDelta::days(30);
        fs::write(trash.manifest_path(&old), serde_json::to_vec(&manifest).unwrap()).unwrap();
        let keep_week = TrashSettings { keep_days: Some(7), ..settings(&trash_dir, None) };
        let _session = TrashSession::new(&keep_week).unwrap();
        assert!(trash.load(&old).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn disabled_trash_has_no_session() {
        assert!(TrashSession::new(&TrashSettings { limit: Some(0), ..TrashSettings::default() }).is_none());
    }

    #[test]
    fn lost_manifest_is_rebuilt_from_folder() {
        let root = temp_dir("rebuild");
        let trash_dir = root.join("Trash");
        let a = junk(&root, r"cache\a.tmp", 10);
        let mut session = TrashSession::new(&settings(&trash_dir, None)).unwrap();
        session.store("user_temp", "Temp", &a, 10, &mut never_asked);
        let id = session.finish().unwrap();
        fs::remove_file(trash_dir.join(format!("{id}.json"))).unwrap();

        let trash = Trash::new(trash_dir);
        let run = trash.load(&id).unwrap();
        assert_eq!(run.items[0].files, [TrashFile { path: a.clone(), size: 10 }]);
        assert_eq!(trash.restore(&id, |_, _| true).restored, 1);
        assert!(a.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
