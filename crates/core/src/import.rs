//! Перенос фото и видео с телефона или карты памяти в папку на компьютере.
//!
//! Источники — то, что система уже показывает как папки: телефоны на KDE через kio-fuse
//! (`mtp:/` — Android, `afc:/` — iPhone), на GNOME — через gvfs (`/run/user/<uid>/gvfs`),
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

#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub name: String,
    pub kind: SourceKind,
    /// Папки `DCIM` на устройстве (у телефона с картой памяти — несколько).
    pub roots: Vec<PathBuf>,
}

// ---------- поиск устройств ----------

/// Подключённые телефоны и карты памяти, на которых есть папка `DCIM`.
/// Пустой список — устройства нет или телефон не разрешил доступ (заблокирован экран,
/// не выбран режим «Передача файлов», на iPhone не нажато «Доверять»).
pub fn find_sources() -> Vec<Source> {
    let mut found = Vec::new();
    #[cfg(target_os = "linux")]
    {
        for (scheme, kind) in [("mtp:/", SourceKind::Phone), ("afc:/", SourceKind::Phone)] {
            if let Some(base) = kio_fuse_mount(scheme) {
                found.extend(devices_in(&base, kind, |n| n.to_string()));
            }
        }
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
                    roots: vec![dcim],
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
                roots: find_dcim(&e.path(), 2),
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
        let Some(out) = run_with_timeout(program, &args, Duration::from_secs(15)) else {
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

#[cfg(target_os = "linux")]
fn run_with_timeout(program: &str, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                return Some(out);
            }
            Ok(Some(_)) => return None,
            Ok(None) if start.elapsed() > limit => {
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
    pub source: PathBuf,
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

fn is_media(path: &Path) -> bool {
    path.extension()
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

/// Что перенести из папок `roots` в `dest`: новые файлы, уже перенесённые — пропустить.
pub fn plan(roots: &[PathBuf], dest: &Path, cancel: &AtomicBool) -> Result<ImportPlan> {
    let mut plan = ImportPlan::default();
    let mut taken: HashSet<PathBuf> = HashSet::new();
    for root in roots {
        let walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !skipped_name(&e.file_name().to_string_lossy()));
        for entry in walker.flatten() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if !entry.file_type().is_file() || !is_media(entry.path()) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let size = meta.len();
            let folder = dest.join(month_folder(&meta));
            let mut target = folder.join(entry.file_name());
            // То же имя и размер — уже перенесён раньше (или дублируется на устройстве).
            if std::fs::metadata(&target).is_ok_and(|m| m.len() == size) {
                plan.already += 1;
                continue;
            }
            let mut n = 1;
            while target.exists() || taken.contains(&target) {
                target = fs_ops::with_suffix(&folder.join(entry.file_name()), n);
                n += 1;
            }
            taken.insert(target.clone());
            plan.bytes += size;
            plan.items.push(ImportItem {
                source: entry.path().to_path_buf(),
                dest: target,
                size,
            });
        }
    }
    Ok(plan)
}

#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub copied: usize,
    pub bytes: u64,
    /// Перенесённые файлы — для «Перевести в JPEG» и истории.
    pub outputs: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
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

fn copy_one(item: &ImportItem, cancel: &AtomicBool, progress: impl Fn(u64)) -> Result<()> {
    let dir = item.dest.parent().expect("у файла назначения есть папка");
    std::fs::create_dir_all(dir).at(dir)?;
    let name = item.dest.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.import-{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut from = std::fs::File::open(&item.source).at(&item.source)?;
        let mut to = std::fs::File::create(&tmp).at(&tmp)?;
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let n = from.read(&mut buf).at(&item.source)?;
            if n == 0 {
                break;
            }
            to.write_all(&buf[..n]).at(&tmp)?;
            done += n as u64;
            progress(done);
        }
        to.sync_all().at(&tmp)?;
        if done != item.size {
            return Err(Error::Verify(format!(
                "скопировано {done} байт из {} — файл на устройстве читается не до конца",
                item.size
            )));
        }
        fs_ops::copy_mtime(&item.source, &tmp)?;
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
        let p = plan(std::slice::from_ref(&phone), &dest, &cancel).unwrap();
        let mut names: Vec<_> = p
            .items
            .iter()
            .map(|i| i.source.file_name().unwrap().to_string_lossy().into_owned())
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
        let again = plan(std::slice::from_ref(&phone), &dest, &cancel).unwrap();
        assert_eq!((again.items.len(), again.already), (0, 2));

        // Другой снимок с тем же именем — рядом, с номером.
        write(&phone.join("Camera/PXL_1.jpg"), b"another photo");
        let third = plan(&[phone], &dest, &cancel).unwrap();
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
