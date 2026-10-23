//! Перенос фото и видео с телефона или карты памяти в папку на компьютере.
//!
//! Источники — то, что уже умеет рабочий стол: на KDE iPhone через kio-fuse (`afc:/` как
//! папка), Android через KIO (`mtp:/` kio-fuse не монтирует — список, сведения и копирование
//! идут через `kioclient`), на GNOME телефоны — через gvfs (`/run/user/<uid>/gvfs`),
//! карты памяти и телефоны в режиме флешки — подключённые диски с папкой `DCIM`.
//! Своих драйверов нет: они спорили бы с рабочим столом за доступ к устройству.
//!
//! С телефона ничего не удаляется. Файлы раскладываются по папкам «ГГГГ-ММ» по дате
//! изменения (у снимков с телефона это время съёмки — дату внутри файла по MTP читать
//! дорого). Уже перенесённое (то же имя и размер) пропускается — перенос можно повторять.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

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
    /// Папки `DCIM` на устройстве (у телефона с картой памяти — несколько).
    pub roots: Vec<Location>,
}

// ---------- поиск устройств ----------

/// Подключённые телефоны и карты памяти, на которых есть папка `DCIM`.
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
/// (MTP: устройство/накопитель/DCIM, iPhone и карта: устройство/DCIM).
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
            Source {
                name: name(&dir_name),
                kind,
                roots: find_dcim(&e.path(), 2)
                    .into_iter()
                    .map(Location::Path)
                    .collect(),
            }
        })
        .collect()
}

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
/// Вывод читается в отдельном потоке: большой список файлов не должен застрять в трубе.
#[cfg(target_os = "linux")]
fn run_with_timeout(
    program: &str,
    args: &[&str],
    limit: Duration,
    cancel: Option<&AtomicBool>,
) -> Option<String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        // Даты в выводе kioclient — на английском при любом языке системы.
        .env("LC_TIME", "C.UTF-8")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).map(|_| out)
    });
    let start = Instant::now();
    loop {
        let stop = cancel.is_some_and(|c| c.load(Ordering::Relaxed));
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return reader.join().ok()?.ok(),
            Ok(Some(_)) => return None,
            Ok(None) if stop || start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
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

fn walk_local(root: &Path, cancel: &AtomicBool, out: &mut Vec<Found>) -> Result<()> {
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !skipped_name(&e.file_name().to_string_lossy()));
    for entry in walker.flatten() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type().is_file() || !is_media(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        out.push(Found {
            location: Location::Path(entry.path().to_path_buf()),
            name,
            size: meta.len(),
            month: month_folder(&meta),
        });
    }
    Ok(())
}

/// Что перенести из папок `roots` в `dest`: новые файлы, уже перенесённые — пропустить.
/// Прогресс — сколько файлов на устройстве уже просмотрено (по MTP это долго).
pub fn plan(
    roots: &[Location],
    dest: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(usize) + Sync,
) -> Result<ImportPlan> {
    let mut found = Vec::new();
    for root in roots {
        match root {
            Location::Path(p) => walk_local(p, cancel, &mut found)?,
            #[cfg(target_os = "linux")]
            Location::Kio(url) => kio::walk(url, cancel, &progress, &mut found)?,
            #[cfg(not(target_os = "linux"))]
            Location::Kio(_) => {}
        }
        progress(found.len());
    }
    let mut plan = ImportPlan::default();
    let mut taken: HashSet<PathBuf> = HashSet::new();
    for f in found {
        let folder = dest.join(&f.month);
        let mut target = folder.join(&f.name);
        // То же имя и размер — уже перенесён раньше (или дублируется на устройстве).
        if std::fs::metadata(&target).is_ok_and(|m| m.len() == f.size) {
            plan.already += 1;
            continue;
        }
        let mut n = 1;
        while target.exists() || taken.contains(&target) {
            target = fs_ops::with_suffix(&folder.join(&f.name), n);
            n += 1;
        }
        taken.insert(target.clone());
        plan.bytes += f.size;
        plan.items.push(ImportItem {
            source: f.location,
            dest: target,
            size: f.size,
        });
    }
    Ok(plan)
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
/// удаляется. Прогресс — `(файлов, байт)`.
pub fn run(plan: &ImportPlan, cancel: &AtomicBool, progress: impl Fn(usize, u64)) -> ImportReport {
    let mut report = ImportReport::default();
    for item in &plan.items {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match copy_one(item, cancel, |b| progress(report.copied, report.bytes + b)) {
            Ok(()) => {
                report.copied += 1;
                report.bytes += item.size;
                report.outputs.push(item.dest.clone());
            }
            Err(Error::Cancelled) => break,
            Err(e) => report.failed.push((item.source.clone(), e.to_string())),
        }
        progress(report.copied, report.bytes);
    }
    report
}

fn copy_local(
    source: &Path,
    tmp: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u64),
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

fn copy_one(item: &ImportItem, cancel: &AtomicBool, progress: impl Fn(u64)) -> Result<()> {
    let dir = item.dest.parent().expect("у файла назначения есть папка");
    std::fs::create_dir_all(dir).at(dir)?;
    let name = item.dest.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.import-{}", std::process::id()));
    let result = (|| -> Result<()> {
        match &item.source {
            Location::Path(p) => copy_local(p, &tmp, cancel, progress)?,
            // KIO сохраняет время изменения сам.
            #[cfg(target_os = "linux")]
            Location::Kio(url) => kio::copy(url, &tmp, item.size, cancel)?,
            #[cfg(not(target_os = "linux"))]
            Location::Kio(_) => {
                return Err(Error::Invalid("адреса KIO есть только на Linux".into()));
            }
        }
        let done = std::fs::metadata(&tmp).at(&tmp)?.len();
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
        std::fs::rename(&tmp, &target).at(&target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// ---------- превью для галереи мастера ----------

/// Видео больше этого — без кадра: качать его целиком ради картинки слишком долго.
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
        Location::Kio(_) => None,
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
    kio::copy(url, &tmp, size, &AtomicBool::new(false)).ok()?;
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

// ---------- Android на KDE: KIO через kioclient ----------

#[cfg(target_os = "linux")]
mod kio {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// Сведения о файле по MTP — по 0,3 с на запуск `kioclient`, поэтому параллельно.
    const PARALLEL: usize = 8;

    fn run(args: &[&str], limit: Duration, cancel: Option<&AtomicBool>) -> Option<String> {
        let mut all = vec!["--noninteractive", "--platform", "offscreen"];
        all.extend_from_slice(args);
        run_with_timeout("kioclient", &all, limit, cancel)
    }

    /// Имена в папке; `None` — телефон не отвечает или `kioclient` нет (не KDE).
    pub(super) fn ls(url: &str) -> Option<Vec<String>> {
        let out = run(&["ls", url], Duration::from_secs(60), None)?;
        Some(
            out.lines()
                .map(str::trim_end)
                .filter(|l| !l.is_empty() && *l != "." && *l != "..")
                .map(str::to_string)
                .collect(),
        )
    }

    /// Адрес вложенного элемента. `%`, `#` и `?` в имени иначе ломают адрес.
    pub(super) fn child(url: &str, name: &str) -> String {
        let name = name
            .replace('%', "%25")
            .replace('#', "%23")
            .replace('?', "%3F");
        format!("{}/{name}", url.trim_end_matches('/'))
    }

    pub(super) struct Stat {
        pub dir: bool,
        pub size: u64,
        pub month: String,
    }

    fn stat(url: &str) -> Option<Stat> {
        let out = run(&["stat", url], Duration::from_secs(30), None)?;
        parse_stat(&out)
    }

    pub(super) fn parse_stat(out: &str) -> Option<Stat> {
        let field = |key: &str| out.lines().find_map(|l| l.strip_prefix(key).map(str::trim));
        Some(Stat {
            // Восьмеричный тип st_mode, с ведущим нулём или без: `0040000` — папка.
            dir: u32::from_str_radix(field("FILE_TYPE")?, 8).ok()? & 0o170000 == 0o040000,
            size: field("SIZE").and_then(|v| v.parse().ok()).unwrap_or(0),
            month: field("MODIFICATION_TIME")
                .and_then(month_of)
                .unwrap_or_else(|| "Без даты".into()),
        })
    }

    /// «Sun Oct 18 15:15:22 2026» → «2026-10». Русские сокращения — на случай другого языка.
    pub(super) fn month_of(time: &str) -> Option<String> {
        const EN: [&str; 12] = [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ];
        const RU: [&str; 12] = [
            "янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
        ];
        let words: Vec<String> = time.split_whitespace().map(str::to_lowercase).collect();
        let year = words
            .iter()
            .find(|w| w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()))?;
        let month = words.iter().find_map(|w| {
            let head: String = w.chars().take(3).collect();
            EN.iter().position(|m| *m == head).or_else(|| {
                RU.iter()
                    .position(|m| *m == head || (*m == "мая" && head == "май"))
            })
        })?;
        Some(format!("{year}-{:02}", month + 1))
    }

    /// Android-телефоны, которые KDE видит по MTP: устройство → накопители → `DCIM`.
    pub(super) fn mtp_sources() -> Vec<Source> {
        let Some(devices) = ls("mtp:/") else {
            return Vec::new();
        };
        devices
            .into_iter()
            .map(|device| {
                let base = child("mtp:/", &device);
                let roots = ls(&base)
                    .unwrap_or_default()
                    .into_iter()
                    .flat_map(|storage| {
                        let storage = child(&base, &storage);
                        ls(&storage)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|n| n.eq_ignore_ascii_case("DCIM"))
                            .map(move |n| Location::Kio(child(&storage, &n)))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                Source {
                    name: device,
                    kind: SourceKind::Phone,
                    roots,
                }
            })
            .collect()
    }

    /// Обход папки на телефоне. Сведения запрашиваются только у папок (без расширения)
    /// и у фото и видео: остальное не нужно.
    pub(super) fn walk(
        root: &str,
        cancel: &AtomicBool,
        progress: &(impl Fn(usize) + Sync),
        out: &mut Vec<Found>,
    ) -> Result<()> {
        let mut queue = vec![root.to_string()];
        let mut first = true;
        while let Some(dir) = queue.pop() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let Some(names) = ls(&dir) else {
                if first {
                    return Err(Error::Invalid(
                        "телефон не отвечает: разблокируйте его и выберите «Передача файлов»"
                            .into(),
                    ));
                }
                continue;
            };
            first = false;
            let wanted: Vec<String> = names
                .into_iter()
                .filter(|n| !skipped_name(n))
                .filter(|n| is_media(n) || Path::new(n).extension().is_none())
                .collect();
            let stats: Mutex<Vec<Option<Stat>>> =
                Mutex::new((0..wanted.len()).map(|_| None).collect());
            let next = AtomicUsize::new(0);
            std::thread::scope(|s| {
                for _ in 0..PARALLEL.min(wanted.len()) {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= wanted.len() || cancel.load(Ordering::Relaxed) {
                                return;
                            }
                            let st = stat(&child(&dir, &wanted[i]));
                            stats.lock().unwrap()[i] = st;
                        }
                    });
                }
            });
            for (name, st) in wanted.into_iter().zip(stats.into_inner().unwrap()) {
                let Some(st) = st else { continue };
                let url = child(&dir, &name);
                if st.dir {
                    queue.push(url);
                } else if is_media(&name) {
                    out.push(Found {
                        location: Location::Kio(url),
                        name,
                        size: st.size,
                        month: st.month,
                    });
                }
            }
            progress(out.len());
        }
        Ok(())
    }

    /// Первые `limit` байт файла с телефона (`kioclient cat`, затем процесс прерывается).
    pub(super) fn head(url: &str, limit: usize) -> Option<Vec<u8>> {
        let mut child = std::process::Command::new("kioclient")
            .args(["--noninteractive", "--platform", "offscreen", "cat", url])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::with_capacity(limit);
            let mut chunk = [0u8; 64 << 10];
            while buf.len() < limit {
                match out.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            buf.truncate(limit);
            buf
        });
        let start = Instant::now();
        while !reader.is_finished() && start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        reader.join().ok().filter(|b| !b.is_empty())
    }

    /// Копия с телефона в `tmp`. KIO сохраняет время изменения. Предел времени — по размеру
    /// (MTP не медленнее ~2 МБ/с), не меньше двух минут.
    pub(super) fn copy(url: &str, tmp: &Path, size: u64, cancel: &AtomicBool) -> Result<()> {
        let _ = std::fs::remove_file(tmp);
        let limit = Duration::from_secs(120 + size / (2 << 20));
        let dest = tmp.to_string_lossy();
        let done = run(&["copy", url, &dest], limit, Some(cancel)).is_some();
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if !done || !tmp.exists() {
            return Err(Error::Invalid(
                "файл не скопировался с телефона — проверьте, что он подключён и разблокирован"
                    .into(),
            ));
        }
        Ok(())
    }
}

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
        let p = plan(&roots, &dest, &cancel, |_| {}).unwrap();
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
        let again = plan(&roots, &dest, &cancel, |_| {}).unwrap();
        assert_eq!((again.items.len(), again.already), (0, 2));

        // Другой снимок с тем же именем — рядом, с номером.
        write(&phone.join("Camera/PXL_1.jpg"), b"another photo");
        let third = plan(&roots, &dest, &cancel, |_| {}).unwrap();
        assert_eq!(third.items.len(), 1);
        assert!(
            third.items[0]
                .dest
                .to_string_lossy()
                .ends_with("PXL_1_1.jpg")
        );
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
