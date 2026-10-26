//! Перенос фото и видео с телефона или карты памяти в папку на компьютере.
//!
//! Источники — то, что уже умеет рабочий стол: на KDE iPhone через kio-fuse (`afc:/` как
//! папка), Android через KIO (`mtp:/` kio-fuse не монтирует — список, сведения и копирование
//! идут через `kioclient`), на GNOME телефоны — через gvfs (`/run/user/<uid>/gvfs`),
//! карты памяти и телефоны в режиме флешки — подключённые диски с папкой `DCIM`.
//! Своих драйверов нет: они спорили бы с рабочим столом за доступ к устройству.
//!
//! У Android снимки лежат не только в `DCIM`: мессенджеры, браузер и ВК кладут их в
//! `Pictures`, `Movies`, `Download`, `Android/media`. Поэтому там просматривается вся
//! память — по тем же правилам, что у галереи телефона: без скрытых папок и папок с
//! `.nomedia`. У iPhone и карт памяти снимки только в `DCIM`.
//!
//! С телефона ничего не удаляется. Файлы раскладываются по папкам «ГГГГ-ММ» по дате
//! изменения (у снимков с телефона это время съёмки — дату внутри файла по MTP читать
//! дорого). Уже перенесённое (то же имя и размер) пропускается — перенос можно повторять.
//!
//! Поиск отдаёт новое по мере нахождения (галерея мастера наполняется сразу), `DCIM` —
//! первой. По желанию сведения о файлах телефона запоминаются ([`plan`], `memory`), и
//! повторный поиск спрашивает телефон только о новом.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{Error, IoContext, Result};
use crate::fs_ops;

/// Что переносить: фото и видео с телефонов и камер.
const MEDIA_EXTENSIONS: &[&str] = &[
    "heic", "heif", "hif", "jpg", "jpeg", "png", "gif", "webp", "dng", "mov", "mp4", "m4v", "3gp",
    "avi", "mkv",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SourceKind {
    /// Телефон (MTP, iPhone, камера по PTP).
    Phone,
    /// Карта памяти или телефон в режиме флешки.
    Card,
}

/// Где лежит файл или папка: обычный путь или адрес KIO (`mtp:/…`, только Linux/KDE).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum Location {
    Path(PathBuf),
    Kio(String),
}

impl Location {
    /// Для журнала и сообщений.
    pub fn display(&self) -> String {
        match self {
            Location::Path(p) => p.display().to_string(),
            Location::Kio(u) => u.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub name: String,
    pub kind: SourceKind,
    /// Где искать: у Android — накопители целиком (у телефона с картой памяти их два),
    /// у iPhone и карт памяти — папки `DCIM`.
    pub roots: Vec<Location>,
}

// ---------- поиск устройств ----------

/// Подключённые телефоны и карты памяти со снимками (с папкой `DCIM`).
/// Пустой список — устройства нет или телефон не разрешил доступ (заблокирован экран,
/// не выбран режим «Передача файлов», на iPhone не нажато «Доверять»).
pub fn find_sources() -> Vec<Source> {
    // Автотесты интерфейса: «телефон» — папка (только отладочная сборка).
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("CONVERTER_TEST_PHONE") {
        return vec![Source {
            name: "Тестовый телефон".into(),
            kind: SourceKind::Phone,
            roots: vec![Location::Path(dir.into())],
        }];
    }
    let mut found = Vec::new();
    #[cfg(target_os = "linux")]
    {
        // iPhone — папкой через kio-fuse; Android — адресами KIO (kio-fuse его не монтирует).
        if let Some(base) = kio_fuse_mount("afc:/") {
            found.extend(devices_in(&base, SourceKind::Phone, |n| n.to_string()));
        }
        found.extend(kio::mtp_sources());
        if let Some(uid) = current_uid() {
            let gvfs = PathBuf::from(format!("/run/user/{uid}/gvfs"));
            found.extend(
                devices_in(&gvfs, SourceKind::Phone, gvfs_name)
                    .into_iter()
                    .filter(|s| !s.roots.is_empty()),
            );
        }
        let user = std::env::var("USER").unwrap_or_default();
        for media in [format!("/run/media/{user}"), format!("/media/{user}")] {
            found.extend(devices_in(Path::new(&media), SourceKind::Card, |n| {
                n.to_string()
            }));
        }
    }
    #[cfg(windows)]
    {
        for letter in b'D'..=b'Z' {
            let root = PathBuf::from(format!("{}:\\", letter as char));
            let dcim = root.join("DCIM");
            if dcim.is_dir() {
                found.push(Source {
                    name: format!("Диск {}:", letter as char),
                    kind: SourceKind::Card,
                    roots: vec![Location::Path(dcim)],
                });
            }
        }
    }
    found.retain(|s| !s.roots.is_empty());
    found
}

/// Подпапки `base` — по устройству на каждую; у каждого ищется `DCIM` на глубине до двух
/// (MTP: устройство/накопитель/DCIM, iPhone и карта: устройство/DCIM). У телефона по MTP
/// (gvfs: `mtp:host=…`) просматривается весь накопитель с этой папкой.
#[cfg(target_os = "linux")]
fn devices_in(base: &Path, kind: SourceKind, name: impl Fn(&str) -> String) -> Vec<Source> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let dir_name = e.file_name().to_string_lossy().into_owned();
            let mtp = dir_name.starts_with("mtp:");
            Source {
                name: name(&dir_name),
                kind,
                roots: find_dcim(&e.path(), 2)
                    .into_iter()
                    .map(|dcim| match dcim.parent() {
                        Some(storage) if mtp => storage.to_path_buf(),
                        _ => dcim,
                    })
                    .map(Location::Path)
                    .collect(),
            }
        })
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn find_dcim(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        if e.file_name().to_string_lossy().eq_ignore_ascii_case("DCIM") {
            found.push(path);
        } else if depth > 1 {
            found.extend(find_dcim(&path, depth - 1));
        }
    }
    found
}

/// `mtp:host=Google_Pixel_7_1A2B3C` → «Google Pixel 7».
#[cfg(target_os = "linux")]
fn gvfs_name(dir: &str) -> String {
    let host = dir.split_once("host=").map_or(dir, |(_, h)| h);
    let mut words: Vec<&str> = host.split('_').filter(|w| !w.is_empty()).collect();
    // Последнее слово у MTP — серийный номер.
    if words.len() > 1
        && words
            .last()
            .is_some_and(|w| w.len() >= 8 && w.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        words.pop();
    }
    words.join(" ")
}

#[cfg(target_os = "linux")]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// Локальный путь к адресу KIO через kio-fuse (KDE): `busctl`, затем `gdbus`.
/// Предел времени: запертый телефон не должен вешать программу.
#[cfg(target_os = "linux")]
fn kio_fuse_mount(url: &str) -> Option<PathBuf> {
    let attempts: [(&str, Vec<&str>); 2] = [
        (
            "busctl",
            vec![
                "--user",
                "call",
                "org.kde.KIOFuse",
                "/org/kde/KIOFuse",
                "org.kde.KIOFuse.VFS",
                "mountUrl",
                "s",
                url,
            ],
        ),
        (
            "gdbus",
            vec![
                "call",
                "--session",
                "--dest",
                "org.kde.KIOFuse",
                "--object-path",
                "/org/kde/KIOFuse",
                "--method",
                "org.kde.KIOFuse.VFS.mountUrl",
                url,
            ],
        ),
    ];
    for (program, args) in attempts {
        let Some(out) = run_with_timeout(program, &args, Duration::from_secs(15), None) else {
            continue;
        };
        // busctl: s "/run/user/1000/kio-fuse-xyz/mtp"; gdbus: ('/run/…',)
        let path: String = out
            .split(['"', '\''])
            .find(|p| p.starts_with('/'))?
            .to_string();
        let path = PathBuf::from(path);
        if path.is_dir() {
            return Some(path);
        }
    }
    None
}

/// Запуск внешней программы с пределом времени; `cancel` — прервать по просьбе человека.
#[cfg(target_os = "linux")]
fn run_with_timeout(
    program: &str,
    args: &[&str],
    limit: Duration,
    cancel: Option<&AtomicBool>,
) -> Option<String> {
    run_watched(program, args, limit, cancel, &|| {})
}

/// То же, `tick` — раз в 50 мс, пока программа работает (прогресс копирования). Вывод
/// читается в отдельном потоке: большой список файлов не должен застрять в трубе; его конец —
/// знак, что программа завершается, — ждать её дальше без опроса раз в 50 мс (на сотнях
/// запусков `kioclient` это заметно).
#[cfg(target_os = "linux")]
fn run_watched(
    program: &str,
    args: &[&str],
    limit: Duration,
    cancel: Option<&AtomicBool>,
    tick: &dyn Fn(),
) -> Option<String> {
    use std::sync::mpsc::RecvTimeoutError;
    let mut child = crate::sys::host_command(program)
        .args(args)
        // Даты в выводе kioclient — на английском при любом языке системы.
        .env("LC_TIME", "C.UTF-8")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = tx.send(stdout.read_to_string(&mut out).map(|_| out));
    });
    let start = Instant::now();
    let over = || cancel.is_some_and(|c| c.load(Ordering::Relaxed)) || start.elapsed() > limit;
    let mut out = None;
    loop {
        if out.is_none() {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(read) => out = Some(read),
                Err(RecvTimeoutError::Timeout) => tick(),
                Err(RecvTimeoutError::Disconnected) => out = Some(Ok(String::new())),
            }
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return match out {
                    Some(read) => read.ok(),
                    None => rx.recv().ok()?.ok(),
                };
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if over() => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) if out.is_some() => std::thread::sleep(Duration::from_millis(2)),
            Ok(None) => {}
        }
    }
}

// ---------- план и перенос ----------

#[derive(Debug, Clone, Serialize)]
pub struct ImportItem {
    pub source: Location,
    pub dest: PathBuf,
    pub size: u64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ImportPlan {
    /// Новые файлы — что перенести.
    pub items: Vec<ImportItem>,
    /// Уже перенесены раньше (то же имя и размер в папке месяца).
    pub already: usize,
    pub bytes: u64,
    /// Сколько папок и файлов телефон не отдал и со второго раза — найдено не всё.
    pub unread: usize,
}

fn is_media(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Служебное: скрытые файлы, корзина Android (`.trashed-…`), миниатюры.
fn skipped_name(name: &str) -> bool {
    name.starts_with('.') || name.eq_ignore_ascii_case("thumbnails")
}

/// Файл, которым Android прячет папку (со всем, что в ней) из галереи: кэш мессенджеров,
/// временные файлы приложений.
const NOMEDIA: &str = ".nomedia";

/// Папки, которых нет в галерее телефона: служебные папки приложений в `Android` (кроме
/// `media`, где фото и видео WhatsApp) и наклейки мессенджеров.
fn skipped_dir(parent: &str, name: &str) -> bool {
    (parent.eq_ignore_ascii_case("Android") && !name.eq_ignore_ascii_case("media"))
        || name.to_lowercase().ends_with("stickers")
}

/// «ГГГГ-ММ» по времени изменения файла.
fn month_folder(meta: &std::fs::Metadata) -> String {
    let secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    let civil = crate::plan::civil_from_unix(secs);
    format!("{}-{}", &civil[..4], &civil[5..7])
}

/// Файл на устройстве: где лежит, размер, папка месяца.
struct Found {
    location: Location,
    name: String,
    size: u64,
    month: String,
}

/// `DCIM` — первой: снимки камеры появляются в галерее мастера раньше остального.
fn dcim_first(name: &std::ffi::OsStr) -> (bool, &std::ffi::OsStr) {
    (!name.eq_ignore_ascii_case("DCIM"), name)
}

fn walk_local(root: &Path, cancel: &AtomicBool, add: &(dyn Fn(Found) + Sync)) -> Result<()> {
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by(|a, b| dcim_first(a.file_name()).cmp(&dcim_first(b.file_name())))
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if !e.file_type().is_dir() {
                return !skipped_name(&name);
            }
            let parent = e.path().parent().and_then(Path::file_name);
            let parent = parent.map(|p| p.to_string_lossy()).unwrap_or_default();
            !skipped_name(&name) && !skipped_dir(&parent, &name) && !e.path().join(NOMEDIA).exists()
        });
    for entry in walker.flatten() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type().is_file() || !is_media(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        add(Found {
            location: Location::Path(entry.path().to_path_buf()),
            name,
            size: meta.len(),
            month: month_folder(&meta),
        });
    }
    Ok(())
}

/// Где запоминаются сведения о файлах телефона (параметр `memory` у [`plan`]).
pub fn memory_path() -> PathBuf {
    crate::paths::cache_dir().join("phone-memory.json")
}

/// Ход поиска для [`plan`]: сколько фото и видео на устройстве просмотрено и, если нашлось
/// новое, — оно с номером в плане (номера не меняются, план только растёт).
pub struct Scan<'a> {
    pub seen: usize,
    pub new: Option<(usize, &'a ImportItem)>,
}

/// Что перенести из папок `roots` в `dest`: новые файлы, уже перенесённые — пропустить.
/// `memory` — файл, где запоминаются сведения о файлах телефона (`None` — не запоминать).
pub fn plan(
    roots: &[Location],
    dest: &Path,
    cancel: &AtomicBool,
    memory: Option<&Path>,
    progress: impl Fn(Scan<'_>) + Sync,
) -> Result<ImportPlan> {
    struct Acc {
        plan: ImportPlan,
        taken: HashSet<PathBuf>,
        seen: usize,
    }
    let acc = Mutex::new(Acc {
        plan: ImportPlan::default(),
        taken: HashSet::new(),
        seen: 0,
    });
    let add = |f: Found| {
        let mut acc = acc.lock().unwrap();
        acc.seen += 1;
        let folder = dest.join(&f.month);
        let mut target = folder.join(&f.name);
        // То же имя и размер — уже перенесён раньше (или дублируется на устройстве).
        if std::fs::metadata(&target).is_ok_and(|m| m.len() == f.size) {
            acc.plan.already += 1;
            progress(Scan {
                seen: acc.seen,
                new: None,
            });
            return;
        }
        let mut n = 1;
        while target.exists() || acc.taken.contains(&target) {
            target = fs_ops::with_suffix(&folder.join(&f.name), n);
            n += 1;
        }
        acc.taken.insert(target.clone());
        acc.plan.bytes += f.size;
        acc.plan.items.push(ImportItem {
            source: f.location,
            dest: target,
            size: f.size,
        });
        let index = acc.plan.items.len() - 1;
        progress(Scan {
            seen: acc.seen,
            new: Some((index, &acc.plan.items[index])),
        });
    };
    #[cfg(target_os = "linux")]
    let remembered = kio::Remembered::open(memory);
    #[cfg(not(target_os = "linux"))]
    let _ = memory;
    for root in roots {
        match root {
            Location::Path(p) => walk_local(p, cancel, &add)?,
            #[cfg(target_os = "linux")]
            Location::Kio(url) => {
                let unread = kio::walk(url, cancel, &remembered, &add)?;
                acc.lock().unwrap().plan.unread += unread;
            }
            #[cfg(not(target_os = "linux"))]
            Location::Kio(_) => {}
        }
    }
    #[cfg(target_os = "linux")]
    remembered.save(roots);
    Ok(acc.into_inner().unwrap().plan)
}

#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub copied: usize,
    pub bytes: u64,
    /// Перенесённые файлы — для «Перевести в JPEG» и истории.
    pub outputs: Vec<PathBuf>,
    pub failed: Vec<(Location, String)>,
}

/// Переносит по плану: копия во временный файл, проверка размера, время изменения как у
/// исходника, затем — настоящее имя. Прервать можно в любой момент: недописанный файл
/// удаляется. Мелкие файлы с телефона — пачками ([`batch_len`]). Прогресс — `(файлов, байт)`.
pub fn run(plan: &ImportPlan, cancel: &AtomicBool, progress: impl Fn(usize, u64)) -> ImportReport {
    let mut report = ImportReport::default();
    let mut rest = &plan.items[..];
    while !rest.is_empty() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let (batch, tail) = rest.split_at(batch_len(rest));
        rest = tail;
        let (copied, bytes) = (report.copied, report.bytes);
        let results = copy_batch(batch, cancel, &|b| progress(copied, bytes + b));
        let mut stop = false;
        for (item, result) in batch.iter().zip(results) {
            match result {
                Ok(target) => {
                    report.copied += 1;
                    report.bytes += item.size;
                    report.outputs.push(target);
                }
                Err(Error::Cancelled) => stop = true,
                Err(e) => report.failed.push((item.source.clone(), e.to_string())),
            }
        }
        progress(report.copied, report.bytes);
        if stop {
            break;
        }
    }
    report
}

/// Сколько первых файлов скопировать одной командой: у телефона на KDE каждый запуск
/// `kioclient` стоит ~50 мс сверх самого копирования (снимки экрана пачкой — втрое быстрее).
fn batch_len(items: &[ImportItem]) -> usize {
    #[cfg(target_os = "linux")]
    if matches!(items[0].source, Location::Kio(_)) {
        return kio::batch_len(items);
    }
    let _ = items;
    1
}

/// Результат по каждому файлу пачки — путь, под которым он лёг.
fn copy_batch(
    batch: &[ImportItem],
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
) -> Vec<Result<PathBuf>> {
    #[cfg(target_os = "linux")]
    if batch.len() > 1 {
        return kio::copy_batch(batch, cancel, progress);
    }
    vec![copy_one(&batch[0], cancel, progress)]
}

fn copy_local(
    source: &Path,
    tmp: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
) -> Result<()> {
    let mut from = std::fs::File::open(source).at(source)?;
    let mut to = std::fs::File::create(tmp).at(tmp)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = from.read(&mut buf).at(source)?;
        if n == 0 {
            break;
        }
        to.write_all(&buf[..n]).at(tmp)?;
        done += n as u64;
        progress(done);
    }
    to.sync_all().at(tmp)?;
    fs_ops::copy_mtime(source, tmp)
}

fn copy_one(item: &ImportItem, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Result<PathBuf> {
    let dir = item.dest.parent().expect("у файла назначения есть папка");
    std::fs::create_dir_all(dir).at(dir)?;
    let name = item.dest.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.import-{}", std::process::id()));
    let result = (|| -> Result<PathBuf> {
        match &item.source {
            Location::Path(p) => copy_local(p, &tmp, cancel, progress)?,
            // KIO сохраняет время изменения сам.
            #[cfg(target_os = "linux")]
            Location::Kio(url) => kio::copy(url, &tmp, item.size, cancel, progress)?,
            #[cfg(not(target_os = "linux"))]
            Location::Kio(_) => {
                return Err(Error::Invalid("адреса KIO есть только на Linux".into()));
            }
        }
        finish(&tmp, item)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Скопированный во временный файл `tmp` — проверить размер и дать настоящее имя.
fn finish(tmp: &Path, item: &ImportItem) -> Result<PathBuf> {
    let done = std::fs::metadata(tmp).at(tmp)?.len();
    if done != item.size {
        return Err(Error::Verify(format!(
            "скопировано {done} байт из {} — файл на устройстве читается не до конца",
            item.size
        )));
    }
    // Имя могло появиться за время переноса — тогда с номером, не перезаписывая.
    let mut target = item.dest.clone();
    let mut n = 1;
    while target.exists() {
        target = fs_ops::with_suffix(&item.dest, n);
        n += 1;
    }
    std::fs::rename(tmp, &target).at(&target)?;
    Ok(target)
}

// ---------- превью для галереи мастера ----------

/// Видео больше этого — без кадра: качать его целиком ради картинки слишком долго.
#[cfg(target_os = "linux")]
const PREVIEW_COPY_LIMIT: u64 = 30 << 20;

/// Файл, по которому можно построить миниатюру (протоколом `thumb://`). Для папки — сам файл;
/// для адреса KIO — маленький JPEG в кэше миниатюр: у JPEG — встроенная миниатюра из первых
/// 256 КБ, у остального фото до 30 МБ — копия, ужатая до 320 px и сразу удалённая.
/// `None` — превью нет (большое видео, телефон не отвечает).
pub fn preview(location: &Location, size: u64) -> Option<PathBuf> {
    match location {
        Location::Path(p) => Some(p.clone()),
        #[cfg(target_os = "linux")]
        Location::Kio(url) => kio_preview(url, size),
        #[cfg(not(target_os = "linux"))]
        Location::Kio(_) => {
            let _ = size;
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn kio_preview(url: &str, size: u64) -> Option<PathBuf> {
    use ::image::GenericImageView;
    let mut key = blake3::Hasher::new();
    key.update(url.as_bytes());
    key.update(&size.to_le_bytes());
    let dir = crate::paths::cache_dir().join("thumbs").join("phone");
    let out = dir.join(format!("{}.jpg", &key.finalize().to_hex()[..32]));
    if out.exists() {
        return Some(out);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let ext = Path::new(url)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if matches!(ext.as_str(), "jpg" | "jpeg")
        && let Some(head) = kio::head(url, 256 << 10)
        && let Some(thumb) = jpeg_exif(&head)
            .and_then(crate::exif::normalize)
            .and_then(|tiff| crate::exif::thumbnail(&tiff).map(<[u8]>::to_vec))
    {
        std::fs::write(&out, thumb).ok()?;
        return Some(out);
    }
    let video = ["mov", "mp4", "m4v", "3gp", "avi", "mkv"].contains(&ext.as_str());
    if video || size > PREVIEW_COPY_LIMIT {
        return None;
    }
    let tmp = dir.join(format!(".{}.{ext}", out.file_stem()?.to_string_lossy()));
    kio::copy(url, &tmp, size, &AtomicBool::new(false), &|_| {}).ok()?;
    let img = match crate::media::detect(&tmp) {
        Some(crate::media::MediaKind::Image(kind)) => crate::thumb::preview(&tmp, kind, 320).ok(),
        _ => None,
    };
    let _ = std::fs::remove_file(&tmp);
    let img = img?;
    let small = if img.dimensions().0.max(img.dimensions().1) > 320 {
        img.thumbnail(320, 320)
    } else {
        img
    };
    small
        .to_rgb8()
        .save_with_format(&out, ::image::ImageFormat::Jpeg)
        .ok()?;
    Some(out)
}

/// Сегмент EXIF (APP1 «Exif\0\0…») из начала JPEG — без чтения всего файла.
#[cfg(any(target_os = "linux", test))]
fn jpeg_exif(head: &[u8]) -> Option<&[u8]> {
    if !head.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    while at + 4 <= head.len() {
        if head[at] != 0xFF {
            return None;
        }
        let marker = head[at + 1];
        if marker == 0xDA || marker == 0xD9 {
            return None; // начались данные картинки
        }
        let len = usize::from(u16::from_be_bytes([head[at + 2], head[at + 3]]));
        let body = head.get(at + 4..at + 2 + len)?;
        if marker == 0xE1 && body.starts_with(b"Exif\0\0") {
            return Some(body);
        }
        at += 2 + len;
    }
    None
}

#[cfg(target_os = "linux")]
mod kio;

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn plans_new_media_skips_service_files_and_imports_once() {
        let dir = tempfile::tempdir().unwrap();
        let phone = dir.path().join("Pixel/Внутренний общий накопитель/DCIM");
        write(&phone.join("Camera/PXL_1.jpg"), b"photo-1");
        write(&phone.join("Camera/PXL_2.mp4"), b"video-22");
        write(&phone.join("Camera/.trashed-123-PXL_0.jpg"), b"in trash");
        write(&phone.join(".thumbnails/1.jpg"), b"thumb");
        write(&phone.join("Camera/notes.txt"), b"text");
        let dest = dir.path().join("Изображения/С телефона");
        let cancel = AtomicBool::new(false);

        assert_eq!(
            find_dcim(&dir.path().join("Pixel"), 2),
            std::slice::from_ref(&phone)
        );
        let roots = [Location::Path(phone.clone())];
        let p = plan(&roots, &dest, &cancel, None, |_| {}).unwrap();
        let mut names: Vec<_> = p
            .items
            .iter()
            .map(|i| match &i.source {
                Location::Path(p) => p.file_name().unwrap().to_string_lossy().into_owned(),
                Location::Kio(u) => u.clone(),
            })
            .collect();
        names.sort();
        assert_eq!(names, ["PXL_1.jpg", "PXL_2.mp4"]);
        assert_eq!(p.bytes, 15);
        // Папка месяца — по времени изменения.
        let month = p.items[0]
            .dest
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(month.len() == 7 && month.as_bytes()[4] == b'-', "{month}");

        let r = run(&p, &cancel, |_, _| {});
        assert_eq!((r.copied, r.bytes, r.failed.len()), (2, 15, 0));
        for out in &r.outputs {
            assert!(out.exists());
            // Временных файлов не осталось.
            assert!(
                std::fs::read_dir(out.parent().unwrap())
                    .unwrap()
                    .flatten()
                    .all(|e| !e.file_name().to_string_lossy().starts_with('.'))
            );
        }

        // Повторный перенос: ничего нового.
        let again = plan(&roots, &dest, &cancel, None, |_| {}).unwrap();
        assert_eq!((again.items.len(), again.already), (0, 2));

        // Другой снимок с тем же именем — рядом, с номером.
        write(&phone.join("Camera/PXL_1.jpg"), b"another photo");
        let third = plan(&roots, &dest, &cancel, None, |_| {}).unwrap();
        assert_eq!(third.items.len(), 1);
        assert!(
            third.items[0]
                .dest
                .to_string_lossy()
                .ends_with("PXL_1_1.jpg")
        );
    }

    #[test]
    fn whole_android_storage_like_phone_gallery() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("Внутренний общий накопитель");
        let wa = storage.join("Android/media/com.whatsapp/WhatsApp/Media");
        write(&storage.join("DCIM/Camera/IMG_1.jpg"), b"1");
        write(&storage.join("Pictures/Telegram/tg.jpg"), b"2");
        write(&storage.join("Movies/VK/vk.mp4"), b"3");
        write(&wa.join("WhatsApp Images/IMG-wa.jpg"), b"4");
        // Галерея телефона этого не показывает.
        write(&wa.join("WhatsApp Images/Private/.nomedia"), b"");
        write(&wa.join("WhatsApp Images/Private/IMG-hidden.jpg"), b"x");
        write(&wa.join("WhatsApp Stickers/s.webp"), b"x");
        write(
            &storage.join("Download/AyuGram/Saved Attachments/.nomedia"),
            b"",
        );
        write(
            &storage.join("Download/AyuGram/Saved Attachments/a.jpg"),
            b"x",
        );
        write(&storage.join("Android/data/com.app/cache/c.jpg"), b"x");

        let cancel = AtomicBool::new(false);
        let found = Mutex::new(Vec::new());
        walk_local(&storage, &cancel, &|f| found.lock().unwrap().push(f.name)).unwrap();
        let names = found.into_inner().unwrap();
        // Камера — первой.
        assert_eq!(names[0], "IMG_1.jpg");
        let mut names = names;
        names.sort();
        assert_eq!(names, ["IMG-wa.jpg", "IMG_1.jpg", "tg.jpg", "vk.mp4"]);
    }

    #[test]
    fn plan_reports_new_items_as_found() {
        let dir = tempfile::tempdir().unwrap();
        let phone = dir.path().join("DCIM");
        write(&phone.join("a.jpg"), b"1");
        write(&phone.join("b.jpg"), b"22");
        let dest = dir.path().join("out");
        let roots = [Location::Path(phone)];
        let streamed = Mutex::new(Vec::new());
        let p = plan(&roots, &dest, &AtomicBool::new(false), None, |scan| {
            if let Some((i, item)) = scan.new {
                streamed.lock().unwrap().push((i, item.dest.clone()));
            }
        })
        .unwrap();
        let streamed = streamed.into_inner().unwrap();
        let fin: Vec<_> = p
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| (i, it.dest.clone()))
            .collect();
        // Номера и места при поиске — те же, что в готовом плане.
        assert_eq!(streamed, fin);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn asks_kio_only_about_media_and_folders() {
        for name in [
            "IMG_1.jpg",
            "VID.MP4",
            "Camera",
            "com.whatsapp",
            "Mr. Smith",
        ] {
            assert!(kio::worth_stat(name), "{name}");
        }
        for name in ["notes.txt", "app.apk", "song.flac", "doc.pdf"] {
            assert!(!kio::worth_stat(name), "{name}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn batches_small_phone_files_of_one_month() {
        let item = |url: &str, month: &str, size: u64| ImportItem {
            source: Location::Kio(format!("mtp:/P/S/DCIM/{url}")),
            dest: PathBuf::from(format!("/out/{month}/{}", kio::file_name(url))),
            size,
        };
        let items = [
            item("Camera/a.jpg", "2026-10", 1 << 20),
            item("Camera/b%23.jpg", "2026-10", 1 << 20),
            // То же имя из другой папки — в одну временную папку не положить.
            item("Screenshots/a.jpg", "2026-10", 1 << 20),
        ];
        assert_eq!(kio::file_name("mtp:/P/S/DCIM/Camera/b%23.jpg"), "b#.jpg");
        assert_eq!(kio::batch_len(&items), 2);
        let other_month = [item("a.jpg", "2026-10", 1), item("b.jpg", "2026-09", 1)];
        assert_eq!(kio::batch_len(&other_month), 1);
        // Большое видео — одно: пачка ограничена по байтам, кроме первого файла.
        let big = [
            item("v.mp4", "2026-10", 900 << 20),
            item("b.jpg", "2026-10", 1),
        ];
        assert_eq!(kio::batch_len(&big), 1);
        let many: Vec<_> = (0..40)
            .map(|i| item(&format!("{i}.jpg"), "2026-10", 1))
            .collect();
        assert_eq!(kio::batch_len(&many), 16);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_kioclient_stat_output() {
        let out = "NAME                  IMG_1.jpg\nSIZE                  3210002\nFILE_TYPE             0100000\n\
                   MODIFICATION_TIME     Sun Oct 18 15:15:22 2026\n";
        let st = kio::parse_stat(out).unwrap();
        assert!(!st.dir);
        assert_eq!((st.size, st.month.as_str()), (3_210_002, "2026-10"));
        let dir = kio::parse_stat("FILE_TYPE             0040000\n").unwrap();
        assert!(dir.dir);
        assert_eq!(dir.month, "Без даты");
        assert_eq!(
            kio::month_of("Вс мая 3 10:00:00 2025").as_deref(),
            Some("2025-05")
        );
        assert_eq!(
            kio::month_of("Mon Jan 1 00:00:00 2024").as_deref(),
            Some("2024-01")
        );
        assert_eq!(kio::month_of("garbage"), None);
        assert_eq!(
            kio::child("mtp:/Redmi/Внутренний общий накопитель/", "50% #1?.jpg"),
            "mtp:/Redmi/Внутренний общий накопитель/50%25 %231%3F.jpg"
        );
    }

    #[test]
    fn finds_exif_segment_in_jpeg_head() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 1, 2];
        jpeg.extend_from_slice(&[0xFF, 0xE1, 0, 10]);
        jpeg.extend_from_slice(b"Exif\0\0II");
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0, 2]);
        assert_eq!(jpeg_exif(&jpeg), Some(&b"Exif\0\0II"[..]));
        assert_eq!(jpeg_exif(&jpeg[..10]), None, "обрезано до APP1");
        assert_eq!(jpeg_exif(b"PNG"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readable_gvfs_names() {
        assert_eq!(
            gvfs_name("mtp:host=Google_Pixel_7_1A2B3C4D"),
            "Google Pixel 7"
        );
        assert_eq!(
            gvfs_name("gphoto2:host=Apple_Inc._iPhone"),
            "Apple Inc. iPhone"
        );
    }
}
