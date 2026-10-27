//! Место на диске: дерево папок с размерами для вкладки «Место на диске».
//!
//! Сканирование обходит папку целиком в фоновых потоках и строит дерево папок с суммарными
//! размерами. Файлы в дереве не хранятся — на диске их бывают миллионы, — а читаются,
//! когда открывают папку ([`entries`]). Символические ссылки и точки соединения (junction)
//! не открываются: так ничего не считается дважды и обход не уходит на другие диски.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::cleaner::is_link;

/// Стек потока сканирования: обход рекурсивный, а вложенность папок бывает глубокой.
const SCAN_STACK_SIZE: usize = 16 * 1024 * 1024;

/// Сколько потоков сканируют папки верхнего уровня.
const MAX_SCAN_THREADS: usize = 8;

/// Папка с суммарным размером всего, что в ней лежит.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DirNode {
    /// Имя папки; у корня сканирования — полный путь.
    pub name: String,
    /// Размер всех файлов внутри, включая вложенные папки, байт.
    pub size: u64,
    /// Число файлов внутри, включая вложенные папки.
    pub files: u64,
    /// Вложенные папки, от больших к меньшим.
    pub children: Vec<DirNode>,
    pub kind: DirKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DirKind {
    #[default]
    Normal,
    /// Символическая ссылка или точка соединения: внутрь не заходим, размер 0.
    Link,
    /// Содержимое не удалось прочитать (обычно нет прав).
    Inaccessible,
}

impl DirNode {
    /// Вложенная папка по относительному пути `relative` (части пути — имена папок).
    pub fn find(&self, relative: &Path) -> Option<&DirNode> {
        let mut node = self;
        for part in relative.components() {
            let name = part.as_os_str().to_string_lossy();
            node = node.children.iter().find(|child| child.name.eq_ignore_ascii_case(&name))?;
        }
        Some(node)
    }
}

/// Элемент списка папки: вложенная папка или файл.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub kind: EntryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// Папка; `files` — число файлов в ней со вложенными.
    Dir { files: u64, kind: DirKind },
    File,
}

/// Содержимое папки `node`, лежащей по пути `path`: вложенные папки из дерева
/// и файлы, прочитанные с диска сейчас. От больших к меньшим.
pub fn entries(node: &DirNode, path: &Path) -> Vec<Entry> {
    let mut entries: Vec<Entry> = node
        .children
        .iter()
        .map(|child| Entry {
            name: child.name.clone(),
            size: child.size,
            kind: EntryKind::Dir { files: child.files, kind: child.kind },
        })
        .collect();
    if let Ok(dir) = fs::read_dir(path) {
        for entry in dir.flatten() {
            // Метаданные из записи каталога: на Windows это не требует отдельного запроса к диску.
            let Ok(metadata) = entry.metadata() else { continue };
            if !metadata.is_dir() {
                entries.push(Entry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    size: metadata.len(),
                    kind: EntryKind::File,
                });
            }
        }
    }
    sort_by_size(&mut entries, |entry| entry.size, |entry| &entry.name);
    entries
}

/// Сколько отсканировано — обновляется на ходу.
#[derive(Default)]
pub struct ScanProgress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub dirs: AtomicU64,
    cancelled: AtomicBool,
}

/// Идущее сканирование.
pub struct ScanJob {
    pub root: PathBuf,
    pub progress: Arc<ScanProgress>,
    receiver: Receiver<DirNode>,
}

impl ScanJob {
    /// Начинает сканирование папки или диска `root` в фоновых потоках.
    pub fn start(root: PathBuf) -> Self {
        let progress = Arc::new(ScanProgress::default());
        let (sender, receiver) = mpsc::channel();
        let thread_progress = Arc::clone(&progress);
        let thread_root = root.clone();
        let spawned = thread::Builder::new().stack_size(SCAN_STACK_SIZE).spawn(move || {
            let _ = sender.send(scan_root(&thread_root, &thread_progress));
        });
        if spawned.is_err() {
            // Поток не запустился — сообщаем пустым результатом, а не зависаем.
            let (sender, failed) = mpsc::channel();
            let _ = sender.send(DirNode { name: root.display().to_string(), kind: DirKind::Inaccessible, ..DirNode::default() });
            return Self { root, progress, receiver: failed };
        }
        Self { root, progress, receiver }
    }

    /// Готовое дерево, если сканирование закончилось (или остановлено — тогда неполное).
    pub fn try_result(&self) -> Option<DirNode> {
        self.receiver.try_recv().ok()
    }

    pub fn cancel(&self) {
        self.progress.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.progress.cancelled.load(Ordering::Relaxed)
    }
}

/// Корень сканируем по-особому: его папки верхнего уровня разбираем несколько потоков.
fn scan_root(root: &Path, progress: &ScanProgress) -> DirNode {
    let mut node = DirNode { name: root.display().to_string(), ..DirNode::default() };
    let Ok(dir) = fs::read_dir(root) else {
        node.kind = DirKind::Inaccessible;
        return node;
    };

    let mut subdirs = Vec::new();
    for entry in dir.flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if metadata.is_dir() {
            subdirs.push(entry.file_name().to_string_lossy().into_owned());
        } else {
            add_file(&mut node, metadata.len(), progress);
        }
    }

    // Потоки берут папки по очереди: одна огромная папка (Windows) не задерживает остальные.
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(subdirs.len()));
    let threads = thread::available_parallelism().map_or(2, |count| count.get()).clamp(1, MAX_SCAN_THREADS);
    thread::scope(|scope| {
        for _ in 0..threads.min(subdirs.len().max(1)) {
            let builder = thread::Builder::new().stack_size(SCAN_STACK_SIZE);
            let _ = builder.spawn_scoped(scope, || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(name) = subdirs.get(index) else { break };
                    let child = scan_dir(&root.join(name), name.clone(), progress);
                    results.lock().unwrap_or_else(|error| error.into_inner()).push(child);
                }
            });
        }
    });

    for child in results.into_inner().unwrap_or_else(|error| error.into_inner()) {
        node.size += child.size;
        node.files += child.files;
        node.children.push(child);
    }
    sort_by_size(&mut node.children, |child| child.size, |child| &child.name);
    node
}

fn scan_dir(path: &Path, name: String, progress: &ScanProgress) -> DirNode {
    let mut node = DirNode { name, ..DirNode::default() };
    progress.dirs.fetch_add(1, Ordering::Relaxed);
    if progress.cancelled.load(Ordering::Relaxed) {
        return node;
    }
    if fs::symlink_metadata(path).is_ok_and(|metadata| is_link(&metadata)) {
        node.kind = DirKind::Link;
        return node;
    }
    let Ok(dir) = fs::read_dir(path) else {
        node.kind = DirKind::Inaccessible;
        return node;
    };

    for entry in dir.flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if metadata.is_dir() {
            let child = scan_dir(&entry.path(), entry.file_name().to_string_lossy().into_owned(), progress);
            node.size += child.size;
            node.files += child.files;
            node.children.push(child);
        } else {
            add_file(&mut node, metadata.len(), progress);
        }
    }
    sort_by_size(&mut node.children, |child| child.size, |child| &child.name);
    node
}

fn add_file(node: &mut DirNode, size: u64, progress: &ScanProgress) {
    node.size += size;
    node.files += 1;
    progress.files.fetch_add(1, Ordering::Relaxed);
    progress.bytes.fetch_add(size, Ordering::Relaxed);
}

/// От больших к меньшим, при равном размере — по имени.
fn sort_by_size<T>(items: &mut [T], size: impl Fn(&T) -> u64, name: impl Fn(&T) -> &str) {
    items.sort_by(|a, b| size(b).cmp(&size(a)).then_with(|| name(a).to_lowercase().cmp(&name(b).to_lowercase())));
}

/// Сектор круговой диаграммы.
#[derive(Clone, Debug, PartialEq)]
pub enum Slice {
    /// Элемент списка с этим номером.
    Item(usize),
    /// Мелкие элементы (номера в списке), объединённые в один сектор.
    Rest(Vec<usize>),
}

/// Секторы для элементов размеров `sizes` (от больших к меньшим): элементы меньше доли
/// `min_share` от суммы объединяются в один сектор, если их больше одного. Пустые не попадают.
pub fn slices(sizes: &[u64], min_share: f64) -> Vec<Slice> {
    let total: u64 = sizes.iter().sum();
    if total == 0 {
        return Vec::new();
    }
    let (big, small): (Vec<usize>, Vec<usize>) = (0..sizes.len())
        .filter(|&index| sizes[index] > 0)
        .partition(|&index| sizes[index] as f64 / total as f64 >= min_share);
    let mut slices: Vec<Slice> = big.into_iter().map(Slice::Item).collect();
    match small.len() {
        0 => {}
        1 => slices.push(Slice::Item(small[0])),
        _ => slices.push(Slice::Rest(small)),
    }
    slices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hexcleaner-disk-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scans_tree_with_sizes_sorted_by_size() {
        let root = temp_dir("scan");
        fs::create_dir_all(root.join("big/inner")).unwrap();
        fs::create_dir_all(root.join("small")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("big/a.bin"), vec![0u8; 3000]).unwrap();
        fs::write(root.join("big/inner/b.bin"), vec![0u8; 2000]).unwrap();
        fs::write(root.join("small/c.bin"), vec![0u8; 100]).unwrap();
        fs::write(root.join("top.bin"), vec![0u8; 1000]).unwrap();

        let progress = ScanProgress::default();
        let tree = scan_root(&root, &progress);
        assert_eq!((tree.size, tree.files), (6100, 4));
        let names: Vec<&str> = tree.children.iter().map(|child| child.name.as_str()).collect();
        assert_eq!(names, ["big", "small", "empty"]);
        assert_eq!((tree.children[0].size, tree.children[0].files), (5000, 2));
        assert_eq!(progress.files.load(Ordering::Relaxed), 4);

        let big = tree.find(Path::new("BIG")).unwrap();
        assert_eq!(big.find(Path::new("inner")).unwrap().size, 2000);
        assert!(tree.find(Path::new("missing")).is_none());

        // Список папки: вложенные папки из дерева и её файлы — вместе, от больших к меньшим.
        let list = entries(big, &root.join("big"));
        let list: Vec<(&str, u64)> = list.iter().map(|entry| (entry.name.as_str(), entry.size)).collect();
        assert_eq!(list, [("a.bin", 3000), ("inner", 2000)]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_folder_is_inaccessible() {
        let tree = scan_root(Path::new(r"C:\no\such\folder\for\hexcleaner"), &ScanProgress::default());
        assert_eq!(tree.kind, DirKind::Inaccessible);
        assert_eq!(tree.size, 0);
    }

    /// Скорость на настоящей папке: `cargo test --release -- --ignored --nocapture scans_windows_folder`.
    #[test]
    #[ignore]
    fn scans_windows_folder() {
        let started = std::time::Instant::now();
        let root = PathBuf::from(std::env::var("WINDIR").unwrap());
        let tree = scan_root(&root, &ScanProgress::default());
        println!("{}: {} files, {} bytes, {:?}", root.display(), tree.files, tree.size, started.elapsed());
        assert!(tree.files > 0);
    }

    #[test]
    fn small_slices_are_merged() {
        // 50 + 30 + 10 + 5 + 3 + 2 = 100.
        let sizes = [50, 30, 10, 5, 3, 2, 0];
        assert_eq!(
            slices(&sizes, 0.04),
            [Slice::Item(0), Slice::Item(1), Slice::Item(2), Slice::Item(3), Slice::Rest(vec![4, 5])]
        );
        // Единственный мелкий элемент остаётся отдельным сектором.
        assert_eq!(slices(&[90, 10], 0.2), [Slice::Item(0), Slice::Item(1)]);
        assert!(slices(&[0, 0], 0.1).is_empty());
    }
}
