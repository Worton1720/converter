//! Поиск дубликатов (раздел 5 ТЗ): точные копии — размер → частичный хэш → полный BLAKE3;
//! «одно фото в разных форматах» — размер в пикселях → отпечаток картинки и дата съёмки (FR-9.2).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::UNIX_EPOCH;

use ::image::DynamicImage;
use ::image::imageops::FilterType;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::error::{Error, IoContext, Result};
use crate::governor::Governor;
use crate::media::{self, ImageKind, MediaKind};
use crate::paths;
use crate::photo;
use crate::scan::canonical;
use crate::sys;
use crate::thumb;

const PARTIAL_CHUNK: u64 = 64 * 1024;
const READERS_SSD: usize = 4;
const READERS_HDD: usize = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KindFilter {
    All,
    Images,
    Videos,
    Extensions(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupesOptions {
    pub roots: Vec<PathBuf>,
    pub recursive: bool,
    pub include_hidden: bool,
    /// Включать облачные файлы, которых нет на диске (FR-7.4).
    pub include_cloud: bool,
    pub min_size: u64,
    pub kinds: KindFilter,
    /// Исключения: путь содержит одну из подстрок.
    pub exclude: Vec<String>,
    pub use_cache: bool,
    /// Искать ещё и одно фото, сохранённое в разных форматах (FR-9.2).
    #[serde(default = "yes")]
    pub find_formats: bool,
}

fn yes() -> bool {
    true
}

impl Default for DupesOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            recursive: true,
            include_hidden: false,
            include_cloud: false,
            min_size: 1024,
            kinds: KindFilter::All,
            exclude: Vec::new(),
            use_cache: true,
            find_formats: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    /// mtime в наносекундах от эпохи — для проверки «не изменился» (FR-11.2).
    pub mtime_ns: i128,
    pub file_id: Option<String>,
    /// Другие имена того же файла (жёсткие ссылки). Удаляются вместе с ним —
    /// иначе место не освободится.
    #[serde(default)]
    pub links: Vec<PathBuf>,
    /// Формат фото — только в группах «одно фото в разных форматах».
    #[serde(default)]
    pub format: Option<ImageKind>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupKind {
    /// Файлы совпадают байт в байт.
    #[default]
    Exact,
    /// Одно и то же фото в разных форматах (HEIC + JPEG и т.п.): содержимое файлов разное.
    Formats,
}

#[derive(Debug, Clone, Serialize)]
pub struct DupGroup {
    #[serde(default)]
    pub kind: GroupKind,
    /// Размер файла; для групп `Formats` — самого большого из них.
    pub size: u64,
    pub hash: String,
    pub files: Vec<FileEntry>,
}

impl DupGroup {
    /// Сколько освободится, если оставить один файл (для `Formats` — самый большой).
    pub fn reclaimable(&self) -> u64 {
        match self.kind {
            GroupKind::Exact => self.size * (self.files.len() as u64).saturating_sub(1),
            GroupKind::Formats => self.files.iter().map(|f| f.size).sum::<u64>() - self.size,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DupesReport {
    pub groups: Vec<DupGroup>,
    /// Жёсткие ссылки на один файл — не дубликаты (FR-7.3).
    pub hardlinks: Vec<Vec<PathBuf>>,
    pub scanned: usize,
    pub skipped_cloud: usize,
    pub errors: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum DupesProgress {
    Scanning {
        found: usize,
    },
    Hashing {
        stage: u8,
        done: usize,
        total: usize,
        bytes_done: u64,
        bytes_total: u64,
    },
}

fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128)
}

fn file_id(path: &Path) -> Option<String> {
    file_id::get_file_id(path).ok().map(|id| format!("{id:?}"))
}

pub fn entry_for(path: &Path) -> Result<FileEntry> {
    let meta = std::fs::metadata(path).at(path)?;
    Ok(FileEntry {
        path: path.to_path_buf(),
        size: meta.len(),
        mtime_ns: mtime_ns(&meta),
        file_id: None,
        links: Vec::new(),
        format: None,
    })
}

// ---------- кэш хэшей (FR-8.2) ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime_ns: i128,
    file_id: Option<String>,
    partial: Option<String>,
    full: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct HashCache {
    entries: HashMap<PathBuf, CacheEntry>,
}

impl HashCache {
    fn path() -> PathBuf {
        paths::cache_dir().join("hashes.json")
    }

    fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(self) {
            // Через временный файл: оборванная запись не должна ломать кэш.
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    fn lookup(&self, e: &FileEntry) -> Option<&CacheEntry> {
        self.entries
            .get(&e.path)
            .filter(|c| c.size == e.size && c.mtime_ns == e.mtime_ns && c.file_id == e.file_id)
    }

    fn store(&mut self, e: &FileEntry, partial: Option<String>, full: Option<String>) {
        let entry = self.entries.entry(e.path.clone()).or_insert(CacheEntry {
            size: e.size,
            mtime_ns: e.mtime_ns,
            file_id: e.file_id.clone(),
            partial: None,
            full: None,
        });
        entry.size = e.size;
        entry.mtime_ns = e.mtime_ns;
        entry.file_id = e.file_id.clone();
        if partial.is_some() {
            entry.partial = partial;
        }
        if full.is_some() {
            entry.full = full;
        }
    }
}

// ---------- хэширование ----------

fn partial_hash(path: &Path, size: u64) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    if size <= 2 * PARTIAL_CHUNK {
        let mut all = Vec::with_capacity(size as usize);
        f.read_to_end(&mut all)?;
        h.update(&all);
    } else {
        let mut buf = vec![0u8; PARTIAL_CHUNK as usize];
        f.read_exact(&mut buf)?;
        h.update(&buf);
        f.seek(SeekFrom::End(-(PARTIAL_CHUNK as i64)))?;
        f.read_exact(&mut buf)?;
        h.update(&buf);
    }
    h.update(&size.to_le_bytes());
    Ok(h.finalize().to_hex().to_string())
}

fn full_hash(path: &Path, bytes_done: &AtomicU64, cancel: &AtomicBool) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("отменено"));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        bytes_done.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(h.finalize().to_hex().to_string())
}

/// Счётчик активных читателей, условная переменная и лимит.
type Gate = Arc<(Mutex<usize>, Condvar, usize)>;

/// Ограничение одновременных читателей на физический диск (раздел 6.3).
#[derive(Default)]
struct DiskGates {
    gates: Mutex<HashMap<String, Gate>>,
    info: Mutex<HashMap<PathBuf, sys::DiskInfo>>,
}

impl DiskGates {
    fn gate(&self, path: &Path) -> Gate {
        let dir = path.parent().unwrap_or(path).to_path_buf();
        let info = self
            .info
            .lock()
            .unwrap()
            .entry(dir.clone())
            .or_insert_with(|| sys::disk_info(&dir))
            .clone();
        let limit = if info.rotational == Some(false) {
            READERS_SSD
        } else {
            READERS_HDD
        };
        Arc::clone(
            self.gates
                .lock()
                .unwrap()
                .entry(info.key)
                .or_insert_with(|| Arc::new((Mutex::new(0), Condvar::new(), limit))),
        )
    }

    fn with<T>(&self, path: &Path, f: impl FnOnce() -> T) -> T {
        let gate = self.gate(path);
        let (lock, cv, limit) = &*gate;
        let mut active = lock.lock().unwrap();
        while *active >= *limit {
            active = cv.wait(active).unwrap();
        }
        *active += 1;
        drop(active);
        let out = f();
        *lock.lock().unwrap() -= 1;
        cv.notify_one();
        out
    }
}

fn matches_kind(path: &Path, filter: &KindFilter) -> bool {
    match filter {
        KindFilter::All => true,
        KindFilter::Extensions(list) => {
            path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                list.iter()
                    .any(|x| x.trim_start_matches('.').eq_ignore_ascii_case(e))
            })
        }
        KindFilter::Images => matches!(media::detect(path), Some(MediaKind::Image(_))),
        KindFilter::Videos => media::detect(path) == Some(MediaKind::Video),
    }
}

/// Хэширует файлы параллельно: токен пула + лимит читателей на диск.
fn hash_parallel(
    files: &[FileEntry],
    governor: &Governor,
    gates: &DiskGates,
    cancel: &AtomicBool,
    hasher: impl Fn(&FileEntry) -> std::io::Result<String> + Sync,
    on_done: impl Fn() + Sync,
) -> Vec<std::io::Result<String>> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<std::io::Result<String>>>> =
        Mutex::new((0..files.len()).map(|_| None).collect());
    let workers = governor.threads_total().min(files.len()).max(1);
    let pool = governor.pool();
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(entry) = files.get(i) else { return };
                    let Some(permit) = pool.acquire(1, cancel) else {
                        return;
                    };
                    sys::set_current_thread_priority(permit.priority());
                    let r = gates.with(&entry.path, || hasher(entry));
                    drop(permit);
                    results.lock().unwrap()[i] = Some(r);
                    on_done();
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err(std::io::Error::other("отменено"))))
        .collect()
}

pub fn find(
    opts: &DupesOptions,
    governor: &Governor,
    cancel: &AtomicBool,
    progress: impl Fn(DupesProgress) + Sync,
) -> Result<DupesReport> {
    let mut report = DupesReport::default();

    // 1. Обход с канонизацией путей: вложенные корни не дают двойного счёта (FR-7.1).
    let mut all: HashMap<PathBuf, FileEntry> = HashMap::new();
    for root in &opts.roots {
        let walker = WalkDir::new(canonical(root))
            .follow_links(false)
            .max_depth(if opts.recursive { usize::MAX } else { 1 })
            .into_iter()
            .filter_entry(|e| {
                let path = e.path().to_string_lossy();
                let excluded = opts
                    .exclude
                    .iter()
                    .any(|x| !x.is_empty() && path.contains(x.as_str()));
                e.depth() == 0
                    || (!excluded
                        && (opts.include_hidden
                            || e.metadata().map_or(true, |m| !sys::is_hidden(e.path(), &m))))
            });
        for entry in walker {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    let path = e.path().map(Path::to_path_buf).unwrap_or_default();
                    report.errors.push((path, e.to_string()));
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() < opts.min_size.max(1) {
                continue;
            }
            if !opts.include_cloud && sys::is_cloud_placeholder(&meta) {
                report.skipped_cloud += 1;
                continue;
            }
            let path = entry.path().to_path_buf();
            if !matches_kind(&path, &opts.kinds) {
                continue;
            }
            all.entry(path.clone()).or_insert(FileEntry {
                path,
                size: meta.len(),
                mtime_ns: mtime_ns(&meta),
                file_id: None,
                links: Vec::new(),
                format: None,
            });
            if all.len().is_multiple_of(1000) {
                progress(DupesProgress::Scanning { found: all.len() });
            }
        }
    }
    report.scanned = all.len();
    progress(DupesProgress::Scanning { found: all.len() });

    // Кандидаты для «одно фото в разных форматах» — до отсева по размеру: их размеры разные.
    let photos: Vec<FileEntry> = if opts.find_formats {
        all.values()
            .filter_map(|e| {
                let kind = photo_kind(&e.path)?;
                Some(FileEntry {
                    format: Some(kind),
                    ..e.clone()
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    // 2. Группировка по размеру, затем схлопывание жёстких ссылок.
    let mut by_size: HashMap<u64, Vec<FileEntry>> = HashMap::new();
    for e in all.into_values() {
        by_size.entry(e.size).or_default().push(e);
    }
    let mut candidates: Vec<FileEntry> = Vec::new();
    for (_, mut files) in by_size.into_iter().filter(|(_, v)| v.len() > 1) {
        let mut by_id: HashMap<String, Vec<FileEntry>> = HashMap::new();
        for mut f in files.drain(..) {
            f.file_id = file_id(&f.path);
            let key = f
                .file_id
                .clone()
                .unwrap_or_else(|| f.path.to_string_lossy().into_owned());
            by_id.entry(key).or_default().push(f);
        }
        let reps: Vec<FileEntry> = by_id
            .into_values()
            .map(|mut same| {
                same.sort_by(|a, b| a.path.cmp(&b.path));
                if same.len() > 1 {
                    report
                        .hardlinks
                        .push(same.iter().map(|f| f.path.clone()).collect());
                }
                let mut rep = same.remove(0);
                rep.links = same.into_iter().map(|f| f.path).collect();
                rep
            })
            .collect();
        if reps.len() > 1 {
            candidates.extend(reps);
        }
    }

    let mut cache = if opts.use_cache {
        HashCache::load()
    } else {
        HashCache::default()
    };
    let gates = DiskGates::default();

    // 3. Частичный хэш.
    let done = AtomicUsize::new(0);
    let total = candidates.len();
    let partial_results = {
        let cache = &cache;
        hash_parallel(
            &candidates,
            governor,
            &gates,
            cancel,
            |e| match cache.lookup(e).and_then(|c| c.partial.clone()) {
                Some(h) => Ok(h),
                None => partial_hash(&e.path, e.size),
            },
            || {
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                progress(DupesProgress::Hashing {
                    stage: 2,
                    done: d,
                    total,
                    bytes_done: 0,
                    bytes_total: 0,
                });
            },
        )
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let mut by_partial: HashMap<(u64, String), Vec<FileEntry>> = HashMap::new();
    for (e, r) in candidates.into_iter().zip(partial_results) {
        match r {
            Ok(h) => {
                cache.store(&e, Some(h.clone()), None);
                by_partial.entry((e.size, h)).or_default().push(e);
            }
            Err(err) => report.errors.push((e.path, err.to_string())),
        }
    }
    let full_candidates: Vec<FileEntry> = by_partial
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();

    // 4. Полный BLAKE3.
    let done = AtomicUsize::new(0);
    let bytes_done = AtomicU64::new(0);
    let total = full_candidates.len();
    let bytes_total: u64 = full_candidates.iter().map(|e| e.size).sum();
    let full_results = {
        let cache = &cache;
        hash_parallel(
            &full_candidates,
            governor,
            &gates,
            cancel,
            |e| match cache.lookup(e).and_then(|c| c.full.clone()) {
                Some(h) => {
                    bytes_done.fetch_add(e.size, Ordering::Relaxed);
                    Ok(h)
                }
                None => full_hash(&e.path, &bytes_done, cancel),
            },
            || {
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                progress(DupesProgress::Hashing {
                    stage: 3,
                    done: d,
                    total,
                    bytes_done: bytes_done.load(Ordering::Relaxed),
                    bytes_total,
                });
            },
        )
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let mut by_full: HashMap<String, Vec<FileEntry>> = HashMap::new();
    for (e, r) in full_candidates.into_iter().zip(full_results) {
        match r {
            Ok(h) => {
                cache.store(&e, None, Some(h.clone()));
                by_full.entry(h).or_default().push(e);
            }
            Err(err) => report.errors.push((e.path, err.to_string())),
        }
    }
    if opts.use_cache {
        cache.save();
    }

    report.groups = by_full
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(hash, mut files)| {
            files.sort_by(|a, b| a.path.cmp(&b.path));
            DupGroup {
                kind: GroupKind::Exact,
                size: files[0].size,
                hash,
                files,
            }
        })
        .collect();

    // 5. Одно фото в разных форматах. Файлы из точных групп сюда не берутся: иначе один файл
    // попал бы в две группы и мог бы быть отмечен дважды. После уборки копий пара найдётся.
    if !photos.is_empty() {
        let in_exact: std::collections::HashSet<&Path> = report
            .groups
            .iter()
            .flat_map(|g| &g.files)
            .flat_map(|f| std::iter::once(&f.path).chain(&f.links))
            .map(PathBuf::as_path)
            .collect();
        let photos: Vec<FileEntry> = photos
            .into_iter()
            .filter(|p| !in_exact.contains(p.path.as_path()))
            .collect();
        let same = find_same_photos(photos, governor, &gates, cancel, &progress)?;
        report.groups.extend(same);
    }
    report.groups.sort_by(|a, b| {
        b.reclaimable()
            .cmp(&a.reclaimable())
            .then(a.hash.cmp(&b.hash))
    });
    Ok(report)
}

// ---------- одно фото в разных форматах (FR-9.2) ----------

const PHOTO_EXTENSIONS: &[&str] = &[
    "heic", "heif", "hif", "jpg", "jpeg", "png", "webp", "tif", "tiff", "bmp",
];
/// Наибольшее расстояние Хэмминга между 64-битными dHash одного фото. Смена формата
/// и пересжатие дают 0–4; у соседних кадров серии того же размера и секунды — заметно больше.
const SAME_PHOTO_DISTANCE: u32 = 6;

/// Тип фото по содержимому; файлы с чужими расширениями не открываются вовсе.
fn photo_kind(path: &Path) -> Option<ImageKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !PHOTO_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    match media::detect(path)? {
        MediaKind::Image(kind) => Some(kind),
        MediaKind::Video => None,
    }
}

/// dHash: картинка 9×8 в оттенках серого, бит — «пиксель ярче правого соседа».
/// Не зависит от формата, сжатия и размера файла.
fn dhash(img: &DynamicImage) -> u64 {
    let small = img
        .thumbnail(256, 256)
        .resize_exact(9, 8, FilterType::Triangle)
        .to_luma8();
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            hash = hash << 1 | u64::from(small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0]);
        }
    }
    hash
}

/// Группы одного фото в разных форматах: одинаковый размер в пикселях с учётом поворота,
/// почти одинаковый отпечаток картинки и одна дата съёмки, если она есть у обоих.
/// Нечитаемые файлы молча пропускаются — это не поиск ошибок.
fn find_same_photos(
    photos: Vec<FileEntry>,
    governor: &Governor,
    gates: &DiskGates,
    cancel: &AtomicBool,
    progress: &(impl Fn(DupesProgress) + Sync),
) -> Result<Vec<DupGroup>> {
    let step = |done: &AtomicUsize, total: usize| {
        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress(DupesProgress::Hashing {
            stage: 4,
            done: d,
            total,
            bytes_done: 0,
            bytes_total: 0,
        });
    };
    let kind = |e: &FileEntry| e.format.expect("у кандидатов формат заполнен");

    // Размер в пикселях и дата съёмки — по заголовку, без декодирования.
    let done = AtomicUsize::new(0);
    let heads = hash_parallel(
        &photos,
        governor,
        gates,
        cancel,
        |e| {
            let (w, h) = photo::dimensions(&e.path, kind(e))
                .ok_or_else(|| std::io::Error::other("размер не читается"))?;
            let date = photo::read_date_taken(&e.path, kind(e)).unwrap_or_default();
            Ok(format!("{w}x{h}|{date}"))
        },
        || step(&done, photos.len()),
    );
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let mut by_dims: HashMap<String, Vec<(FileEntry, String)>> = HashMap::new();
    for (e, head) in photos.into_iter().zip(heads) {
        if let Some((dims, date)) = head.ok().as_deref().and_then(|h| h.split_once('|')) {
            by_dims
                .entry(dims.to_string())
                .or_default()
                .push((e, date.to_string()));
        }
    }
    // Отпечаток считается только у файлов, у которых есть возможная пара: другой формат
    // того же размера и той же даты (или без даты). В фототеке с тысячами снимков 4032×3024
    // это единицы файлов, а не все.
    let pairable = |a: &(FileEntry, String), b: &(FileEntry, String)| {
        a.0.format != b.0.format && dates_match(&a.1, &b.1)
    };
    let buckets: Vec<Vec<(FileEntry, String)>> = by_dims
        .into_values()
        .map(|v| {
            v.iter()
                .filter(|a| v.iter().any(|b| pairable(a, b)))
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .collect();
    let candidates: Vec<FileEntry> = buckets.iter().flatten().map(|(e, _)| e.clone()).collect();

    // Отпечаток уменьшенной картинки.
    let done = AtomicUsize::new(0);
    let prints = hash_parallel(
        &candidates,
        governor,
        gates,
        cancel,
        |e| {
            let img = thumb::preview(&e.path, kind(e), 64).map_err(std::io::Error::other)?;
            Ok(format!("{:016x}", dhash(&img)))
        },
        || step(&done, candidates.len()),
    );
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }

    let mut prints = prints.into_iter();
    let mut groups = Vec::new();
    for bucket in buckets {
        let items: Vec<(FileEntry, u64, String)> = bucket
            .into_iter()
            .zip(prints.by_ref())
            .filter_map(|((e, date), p)| Some((e, u64::from_str_radix(&p.ok()?, 16).ok()?, date)))
            .collect();
        groups.extend(cluster_same_photos(items));
    }
    Ok(groups)
}

/// Дата съёмки совпадает или неизвестна у одного из файлов (метаданные могли убрать).
fn dates_match(a: &str, b: &str) -> bool {
    a.is_empty() || b.is_empty() || a == b
}

/// Связывает одно фото в разных форматах и оставляет группы, где есть хотя бы два формата.
/// Файлы с датой съёмки связываются только с файлами той же даты. Файл без даты
/// присоединяется к одному, самому похожему: иначе он склеил бы соседние кадры серии
/// с разными секундами в одну группу.
fn cluster_same_photos(items: Vec<(FileEntry, u64, String)>) -> Vec<DupGroup> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let close = |a: &(FileEntry, u64, String), b: &(FileEntry, u64, String)| {
        a.0.format != b.0.format && (a.1 ^ b.1).count_ones() <= SAME_PHOTO_DISTANCE
    };
    let link = |parent: &mut [usize], i: usize, j: usize| {
        let (ri, rj) = (root(parent, i), root(parent, j));
        parent[ri] = rj;
    };
    for i in 0..n {
        for j in i + 1..n {
            let (a, b) = (&items[i], &items[j]);
            if !a.2.is_empty() && a.2 == b.2 && close(a, b) {
                link(&mut parent, i, j);
            }
        }
    }
    for i in (0..n).filter(|&i| items[i].2.is_empty()) {
        let best = (0..n)
            .filter(|&j| j != i && close(&items[i], &items[j]))
            .min_by_key(|&j| (items[i].1 ^ items[j].1).count_ones());
        if let Some(j) = best {
            link(&mut parent, i, j);
        }
    }
    let mut components: HashMap<usize, Vec<(FileEntry, u64)>> = HashMap::new();
    for (i, (e, hash, _)) in items.into_iter().enumerate() {
        components
            .entry(root(&mut parent, i))
            .or_default()
            .push((e, hash));
    }
    components
        .into_values()
        .filter(|c| c.iter().any(|(e, _)| e.format != c[0].0.format))
        .map(|mut c| {
            c.sort_by(|a, b| a.0.path.cmp(&b.0.path));
            let hash = format!("{:016x}", c[0].1);
            let files: Vec<FileEntry> = c.into_iter().map(|(e, _)| e).collect();
            DupGroup {
                kind: GroupKind::Formats,
                size: files.iter().map(|f| f.size).max().unwrap_or(0),
                hash,
                files,
            }
        })
        .collect()
}

// ---------- выбор и действия (FR-10.3–10.5, 11.x) ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeepRule {
    Oldest,
    Newest,
    ShortestPath,
    /// Оставить файл из первой по порядку папки.
    PriorityFolders(Vec<PathBuf>),
}

/// Отметки «удалить»: в группе всегда остаётся ровно один неотмеченный файл.
/// Точные копии — по правилу `rule`; одно фото в разных форматах — файл формата `prefer`
/// (FR-9.2), а если такого нет или их несколько — самый большой: в нём больше деталей.
pub fn auto_select(group: &DupGroup, rule: &KeepRule, prefer: ImageKind) -> Vec<bool> {
    let path_len = |f: &FileEntry| f.path.as_os_str().len();
    let keeper = (0..group.files.len())
        .min_by(|&a, &b| {
            let (fa, fb) = (&group.files[a], &group.files[b]);
            let primary = match (group.kind, rule) {
                (GroupKind::Formats, _) => (fa.format != Some(prefer))
                    .cmp(&(fb.format != Some(prefer)))
                    .then(fb.size.cmp(&fa.size)),
                (_, KeepRule::Oldest) => fa.mtime_ns.cmp(&fb.mtime_ns),
                (_, KeepRule::Newest) => fb.mtime_ns.cmp(&fa.mtime_ns),
                (_, KeepRule::ShortestPath) => path_len(fa).cmp(&path_len(fb)),
                (_, KeepRule::PriorityFolders(dirs)) => {
                    let rank = |f: &FileEntry| {
                        dirs.iter()
                            .position(|d| f.path.starts_with(canonical(d)))
                            .unwrap_or(usize::MAX)
                    };
                    rank(fa).cmp(&rank(fb))
                }
            };
            primary
                .then(path_len(fa).cmp(&path_len(fb)))
                .then(fa.path.cmp(&fb.path))
        })
        .unwrap_or(0);
    (0..group.files.len()).map(|i| i != keeper).collect()
}

/// Инвариант FR-10.4: хотя бы один файл группы остаётся.
pub fn validate(group: &DupGroup, marks: &[bool]) -> Result<()> {
    if marks.len() != group.files.len() {
        return Err(Error::Invalid("отметки не соответствуют группе".into()));
    }
    if marks.iter().all(|&m| m) {
        return Err(Error::Invalid(format!(
            "в группе {} отмечены все файлы — хотя бы один должен остаться",
            &group.hash[..12.min(group.hash.len())]
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DupAction {
    Trash,
    Delete,
    MoveTo(PathBuf),
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub path: PathBuf,
    pub error: Option<String>,
}

fn unchanged(e: &FileEntry) -> bool {
    std::fs::metadata(&e.path).is_ok_and(|m| m.len() == e.size && mtime_ns(&m) == e.mtime_ns)
}

/// Выполняет действие над отмеченными файлами. Перед каждым удалением проверяется,
/// что файл не изменился и что оставляемый файл группы на месте (FR-11.2).
pub fn apply(
    groups: &[DupGroup],
    marks: &[Vec<bool>],
    action: &DupAction,
) -> Result<Vec<ActionResult>> {
    if groups.len() != marks.len() {
        return Err(Error::Invalid("отметки не соответствуют группам".into()));
    }
    for (g, m) in groups.iter().zip(marks) {
        validate(g, m)?;
    }
    let mut results = Vec::new();
    for (group, marks) in groups.iter().zip(marks) {
        let keeper_ok = group
            .files
            .iter()
            .zip(marks)
            .any(|(f, &marked)| !marked && unchanged(f));
        for (file, _) in group.files.iter().zip(marks).filter(|(_, m)| **m) {
            let error = if !keeper_ok {
                Some("оставляемый файл группы исчез или изменился — пропущено".to_string())
            } else if !unchanged(file) {
                Some(Error::ChangedSinceScan(file.path.clone()).to_string())
            } else {
                None
            };
            let names = std::iter::once(&file.path).chain(&file.links);
            for path in names {
                let error = error.clone().or_else(|| match action {
                    DupAction::Trash => crate::fs_ops::trash(path).err().map(|e| e.to_string()),
                    DupAction::Delete => std::fs::remove_file(path).err().map(|e| e.to_string()),
                    DupAction::MoveTo(dir) => move_into(path, dir).err().map(|e| e.to_string()),
                });
                results.push(ActionResult {
                    path: path.clone(),
                    error,
                });
            }
        }
    }
    Ok(results)
}

fn move_into(path: &Path, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).at(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::Invalid("нет имени файла".into()))?;
    let mut target = dir.join(name);
    let mut n = 1;
    while target.exists() {
        target = crate::fs_ops::with_suffix(&dir.join(name), n);
        n += 1;
    }
    if std::fs::rename(path, &target).is_err() {
        // Другой том: копия с проверкой размера, затем удаление.
        std::fs::copy(path, &target).at(&target)?;
        let ok = std::fs::metadata(&target)
            .is_ok_and(|m| Some(m.len()) == std::fs::metadata(path).ok().map(|m| m.len()));
        if !ok {
            let _ = std::fs::remove_file(&target);
            return Err(Error::Verify("копия не совпала по размеру".into()));
        }
        std::fs::remove_file(path).at(path)?;
    }
    Ok(target)
}

/// Отчёт CSV (FR-11.4).
pub fn to_csv(groups: &[DupGroup], marks: Option<&[Vec<bool>]>) -> String {
    let mut out = String::from("group,type,hash,size,path,mtime_ns,action\n");
    for (gi, g) in groups.iter().enumerate() {
        for (fi, f) in g.files.iter().enumerate() {
            let marked = marks
                .and_then(|m| m.get(gi))
                .and_then(|m| m.get(fi))
                .copied()
                .unwrap_or(false);
            let path = f.path.to_string_lossy().replace('"', "\"\"");
            out.push_str(&format!(
                "{},{},{},{},\"{}\",{},{}\n",
                gi + 1,
                match g.kind {
                    GroupKind::Exact => "exact",
                    GroupKind::Formats => "formats",
                },
                g.hash,
                f.size,
                path,
                f.mtime_ns,
                if marked { "remove" } else { "keep" }
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governor::LoadLevel;

    fn write(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    fn opts(root: &Path) -> DupesOptions {
        DupesOptions {
            roots: vec![root.to_path_buf()],
            min_size: 1,
            use_cache: false,
            ..Default::default()
        }
    }

    #[test]
    fn finds_exact_duplicates_and_ignores_same_size_and_hardlinks() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut big_other = big.clone();
        big_other[150_000] ^= 1; // та же голова и хвост, отличие в середине
        write(&d.join("a/1.bin"), &big);
        write(&d.join("b/2.bin"), &big);
        write(&d.join("c/3.bin"), &big_other);
        write(&d.join("small1"), b"hello");
        write(&d.join("small2"), b"world");
        #[cfg(unix)]
        std::fs::hard_link(d.join("a/1.bin"), d.join("a/1-link.bin")).unwrap();

        let gov = Governor::start(LoadLevel::Max, 0);
        let cancel = AtomicBool::new(false);
        // Вложенный корень не должен давать двойного счёта.
        let mut o = opts(d);
        o.roots.push(d.join("a"));
        let report = find(&o, &gov, &cancel, |_| {}).unwrap();
        assert_eq!(report.groups.len(), 1, "{:?}", report.groups);
        let names: Vec<_> = report.groups[0]
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        #[cfg(unix)]
        {
            assert_eq!(report.hardlinks.len(), 1);
            assert_eq!(names.len(), 2);
            let with_link = report.groups[0]
                .files
                .iter()
                .find(|f| !f.links.is_empty())
                .unwrap();
            assert_eq!(with_link.links.len(), 1);
        }
        assert!(names.contains(&"2.bin".to_string()));
        gov.shutdown();
    }

    fn group(paths: &[(&str, i128)]) -> DupGroup {
        DupGroup {
            kind: GroupKind::Exact,
            size: 10,
            hash: "abcdef0123456789".into(),
            files: paths
                .iter()
                .map(|(p, t)| FileEntry {
                    path: PathBuf::from(p),
                    size: 10,
                    mtime_ns: *t,
                    file_id: None,
                    links: Vec::new(),
                    format: None,
                })
                .collect(),
        }
    }

    fn picture(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> ::image::RgbImage {
        ::image::RgbImage::from_fn(w, h, |x, y| {
            let v = f(x, y);
            ::image::Rgb([v, v / 2, 255 - v])
        })
    }

    #[test]
    fn finds_same_photo_in_different_formats() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let photo = picture(320, 240, |x, y| ((x + y) % 256) as u8);
        photo.save(d.join("IMG_1.png")).unwrap();
        photo.save(d.join("IMG_1.jpg")).unwrap();
        // Другой снимок того же размера и формата PNG — в группу не попадает.
        picture(
            320,
            240,
            |x, y| if (x / 40 + y / 40) % 2 == 0 { 30 } else { 220 },
        )
        .save(d.join("IMG_2.png"))
        .unwrap();
        // Тот же снимок другого размера — тоже не пара.
        ::image::imageops::thumbnail(&photo, 160, 120)
            .save(d.join("small.jpg"))
            .unwrap();

        let gov = Governor::start(LoadLevel::Max, 0);
        let cancel = AtomicBool::new(false);
        let report = find(&opts(d), &gov, &cancel, |_| {}).unwrap();
        gov.shutdown();
        assert_eq!(report.groups.len(), 1, "{:?}", report.groups);
        let g = &report.groups[0];
        assert_eq!(g.kind, GroupKind::Formats);
        let names: Vec<_> = g
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["IMG_1.jpg", "IMG_1.png"]);
        // Оставляется JPEG, отмечается PNG.
        assert_eq!(
            auto_select(g, &KeepRule::Oldest, ImageKind::Jpeg),
            [false, true]
        );
        assert_eq!(g.reclaimable(), g.files[1].size.min(g.files[0].size));

        let mut o = opts(d);
        o.find_formats = false;
        let gov = Governor::start(LoadLevel::Max, 0);
        assert!(find(&o, &gov, &cancel, |_| {}).unwrap().groups.is_empty());
        gov.shutdown();
    }

    #[test]
    fn same_photos_need_matching_dates() {
        let entry = |name: &str, format| FileEntry {
            path: PathBuf::from(name),
            size: 1,
            mtime_ns: 0,
            file_id: None,
            links: Vec::new(),
            format: Some(format),
        };
        let groups = cluster_same_photos(vec![
            (
                entry("/a.heic", ImageKind::Heif),
                0xF0F0,
                "2024:05:01 10:00:00".into(),
            ),
            (
                entry("/a.jpg", ImageKind::Jpeg),
                0xF0F1,
                "2024:05:01 10:00:00".into(),
            ),
            // Тот же отпечаток, но другая секунда — соседний кадр серии.
            (
                entry("/b.jpg", ImageKind::Jpeg),
                0xF0F0,
                "2024:05:01 10:00:01".into(),
            ),
            // Даты нет — метаданные убраны при конвертации; сравнивается по картинке.
            (entry("/a.png", ImageKind::Png), 0xF0F0, String::new()),
        ]);
        assert_eq!(groups.len(), 1);
        let mut names: Vec<_> = groups[0].files.iter().map(|f| f.path.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                Path::new("/a.heic"),
                Path::new("/a.jpg"),
                Path::new("/a.png")
            ]
        );
    }

    #[test]
    fn formats_keep_preferred_then_largest() {
        let file = |name: &str, format, size| FileEntry {
            path: PathBuf::from(name),
            size,
            mtime_ns: 0,
            file_id: None,
            links: Vec::new(),
            format: Some(format),
        };
        let g = DupGroup {
            kind: GroupKind::Formats,
            size: 3_000,
            hash: "0".into(),
            files: vec![
                file("/a.heic", ImageKind::Heif, 2_000),
                file("/a.jpg", ImageKind::Jpeg, 1_000),
                file("/a.png", ImageKind::Png, 3_000),
            ],
        };
        assert_eq!(
            auto_select(&g, &KeepRule::Oldest, ImageKind::Jpeg),
            [true, false, true]
        );
        assert_eq!(
            auto_select(&g, &KeepRule::Oldest, ImageKind::Heif),
            [false, true, true]
        );
        // Нужного формата нет — остаётся самый большой файл.
        assert_eq!(
            auto_select(&g, &KeepRule::Oldest, ImageKind::Webp),
            [true, true, false]
        );
        assert_eq!(g.reclaimable(), 3_000);
    }

    #[test]
    fn auto_select_keeps_exactly_one() {
        let g = group(&[("/x/long/path/a", 30), ("/x/b", 10), ("/x/c", 20)]);
        assert_eq!(
            auto_select(&g, &KeepRule::Oldest, ImageKind::Jpeg),
            [true, false, true]
        );
        assert_eq!(
            auto_select(&g, &KeepRule::Newest, ImageKind::Jpeg),
            [false, true, true]
        );
        assert_eq!(
            auto_select(&g, &KeepRule::ShortestPath, ImageKind::Jpeg),
            [true, false, true]
        );
        let prio = KeepRule::PriorityFolders(vec![PathBuf::from("/x/long")]);
        assert_eq!(auto_select(&g, &prio, ImageKind::Jpeg), [false, true, true]);
        for rule in [KeepRule::Oldest, KeepRule::Newest, KeepRule::ShortestPath] {
            let m = auto_select(&g, &rule, ImageKind::Jpeg);
            assert_eq!(m.iter().filter(|x| !**x).count(), 1);
        }
    }

    #[test]
    fn apply_refuses_to_remove_whole_group_and_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        write(&a, b"same");
        write(&b, b"same");
        let g = DupGroup {
            kind: GroupKind::Exact,
            size: 4,
            hash: "h".repeat(64),
            files: vec![entry_for(&a).unwrap(), entry_for(&b).unwrap()],
        };
        assert!(
            apply(
                std::slice::from_ref(&g),
                &[vec![true, true]],
                &DupAction::Delete
            )
            .is_err()
        );
        assert!(a.exists() && b.exists());

        // Файл изменился после сканирования — не удаляем.
        write(&b, b"changed!");
        let r = apply(
            std::slice::from_ref(&g),
            &[vec![false, true]],
            &DupAction::Delete,
        )
        .unwrap();
        assert!(r[0].error.is_some());
        assert!(b.exists());

        // Оставляемый файл исчез — не удаляем последнюю копию.
        let g2 = DupGroup {
            files: vec![entry_for(&a).unwrap(), entry_for(&b).unwrap()],
            ..g
        };
        std::fs::remove_file(&a).unwrap();
        let r = apply(
            std::slice::from_ref(&g2),
            &[vec![false, true]],
            &DupAction::Delete,
        )
        .unwrap();
        assert!(r[0].error.is_some());
        assert!(b.exists());
    }

    #[test]
    fn move_into_avoids_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.jpg");
        let dest = dir.path().join("dest");
        write(&src, b"1");
        write(&dest.join("x.jpg"), b"2");
        let moved = move_into(&src, &dest).unwrap();
        assert_eq!(moved, dest.join("x_1.jpg"));
        assert_eq!(std::fs::read(dest.join("x.jpg")).unwrap(), b"2");
    }
}
