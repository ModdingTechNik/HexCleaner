//! Перенос файлов в корзину во время очистки.

use std::path::{Path, PathBuf};

use chrono::Local;

use super::{Compression, Trash, TrashFile, TrashItem, TrashPolicy, TrashRun, TrashSettings, mirror_path};
use crate::system::compact;
use crate::system::delete::{self, DeleteError};

/// Файл не помещается в лимит корзины — что делать.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitRequest {
    /// Сколько уже перенесено в этой очистке плюс размер файла, байт.
    pub needed: u64,
    /// Сколько занимают прошлые очистки, байт.
    pub used: u64,
    pub limit: u64,
}

/// Ответ на превышение лимита. Действует до конца очистки.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrashDecision {
    /// Переносить в корзину, не глядя на лимит.
    IgnoreLimit,
    /// Удалять самые старые очистки из корзины, пока новые файлы не поместятся.
    /// Если место не найдётся, файлы удаляются мимо корзины.
    FreeSpace,
    /// Файлы, которые не помещаются, удалять мимо корзины.
    DeletePermanently,
    /// Остановить очистку.
    Stop,
}

/// Что стало с файлом.
#[derive(Debug)]
pub enum Stored {
    Moved,
    /// В корзину не переносим — файл нужно удалить.
    NotMoved,
    /// Перенести не удалось: файл занят, нет прав и т. п.
    Failed(DeleteError),
    /// Пользователь остановил очистку.
    Stopped,
}

/// Перенос в корзину во время одной очистки. Папка очистки создаётся при первом файле.
pub struct TrashSession {
    trash: Trash,
    settings: TrashSettings,
    run: Option<TrashRun>,
    /// Сколько занимают прошлые очистки; считается при первой проверке лимита.
    used: Option<u64>,
    /// Сколько перенесено в этой очистке.
    added: u64,
    decision: Option<TrashDecision>,
}

impl TrashSession {
    /// `None`, если корзина отключена. Очистки старше срока хранения удаляются сразу.
    pub fn new(settings: &TrashSettings) -> Option<Self> {
        if !settings.is_enabled() {
            return None;
        }
        let trash = Trash::new(settings.directory());
        if let Some(days) = settings.keep_days {
            trash.remove_expired(days, Local::now());
        }
        Some(Self { trash, settings: settings.clone(), run: None, used: None, added: 0, decision: None })
    }

    pub fn directory(&self) -> &Path {
        self.trash.dir()
    }

    /// Переносит файл пункта `task_id` в корзину по его политике и лимиту.
    /// Если лимит превышен и ответа ещё нет, спрашивает `ask`.
    pub fn store(
        &mut self,
        task_id: &str,
        task_title: &str,
        path: &Path,
        size: u64,
        ask: &mut dyn FnMut(LimitRequest) -> TrashDecision,
    ) -> Stored {
        match self.settings.policy(task_id) {
            TrashPolicy::Never => return Stored::NotMoved,
            TrashPolicy::Always => {}
            TrashPolicy::Normal => {
                if let Some(limit) = self.settings.limit
                    && !self.fits(size, limit)
                {
                    let request = LimitRequest { needed: self.added + size, used: self.used.unwrap_or(0), limit };
                    let decision = *self.decision.get_or_insert_with(|| ask(request));
                    match decision {
                        TrashDecision::IgnoreLimit => {}
                        TrashDecision::FreeSpace => {
                            let keep = self.run.as_ref().map(|run| run.id.clone());
                            let freed = self.trash.free_space(self.added + size, limit, keep.as_deref());
                            self.used = None;
                            if !freed {
                                return Stored::NotMoved;
                            }
                        }
                        TrashDecision::DeletePermanently => return Stored::NotMoved,
                        TrashDecision::Stop => return Stored::Stopped,
                    }
                }
            }
        }
        self.move_in(task_id, task_title, path, size)
    }

    /// Поместится ли ещё `size` байт в лимит.
    fn fits(&mut self, size: u64, limit: u64) -> bool {
        let keep = self.run.as_ref().map(|run| run.id.clone());
        let used = *self.used.get_or_insert_with(|| {
            self.trash.runs().iter().filter(|run| Some(&run.id) != keep.as_ref()).map(|run| run.stored_bytes).sum()
        });
        used + self.added + size <= limit
    }

    fn move_in(&mut self, task_id: &str, task_title: &str, path: &Path, size: u64) -> Stored {
        let Some(relative) = mirror_path(path) else { return Stored::NotMoved };
        let run_dir = match self.run_dir() {
            Some(dir) => dir,
            None => return Stored::NotMoved,
        };
        let target = run_dir.join(relative);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = delete::move_file(path, &target) {
            return Stored::Failed(error);
        }

        self.added += size;
        let Some(run) = self.run.as_mut() else { return Stored::NotMoved };
        let file = TrashFile { path: path.to_owned(), size };
        match run.items.iter_mut().find(|item| item.task == task_id) {
            Some(item) => item.files.push(file),
            None => run.items.push(TrashItem { task: task_id.to_owned(), title: task_title.to_owned(), files: vec![file] }),
        }
        Stored::Moved
    }

    /// Папка этой очистки в корзине; при первом вызове создаётся.
    fn run_dir(&mut self) -> Option<PathBuf> {
        if self.run.is_none() {
            let now = Local::now();
            let base = now.format("%Y-%m-%d %H-%M-%S").to_string();
            // Две очистки в одну секунду получают разные папки.
            let id = (1..)
                .map(|number| if number == 1 { base.clone() } else { format!("{base} ({number})") })
                .find(|id| !self.trash.run_dir(id).exists() && !self.trash.manifest_path(id).exists())?;
            std::fs::create_dir_all(self.trash.run_dir(&id)).ok()?;
            self.run = Some(TrashRun { id, created: now, items: Vec::new(), archived: false, stored_bytes: 0 });
        }
        self.run.as_ref().map(|run| self.trash.run_dir(&run.id))
    }

    /// Записывает описание очистки — после каждого пункта, чтобы при сбое не потерять, что откуда.
    pub fn save(&self) {
        if let Some(run) = &self.run
            && !run.items.is_empty()
        {
            let _ = self.trash.save(run);
        }
    }

    /// Очистка закончилась: описание записывается, папка сжимается. Возвращает имя очистки
    /// в корзине, если в неё что-то попало.
    pub fn finish(self) -> Option<String> {
        let run = self.run.as_ref()?;
        if run.items.is_empty() {
            let _ = std::fs::remove_dir_all(self.trash.run_dir(&run.id));
            return None;
        }
        self.save();
        if self.settings.compress {
            match self.settings.compression {
                Compression::Zip => {
                    let _ = self.trash.pack(&run.id);
                }
                // Сжатие идёт в фоне — очистку оно не задерживает.
                Compression::Ntfs => {
                    let _ = compact::compress_in_background(&self.trash.run_dir(&run.id));
                }
            }
        }
        Some(run.id.clone())
    }
}
