//! Обход входных путей (FR-1.1–1.3), пробы видео и Live Photo (FR-1.7).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde::Serialize;
use walkdir::WalkDir;

use crate::ffmpeg::Tools;
use crate::media::{self, MediaKind};
use crate::probe::{self, VideoInfo};
use crate::sys;

/// Видео Live Photo короче этого порога.
const LIVE_PHOTO_MAX_SECS: f64 = 4.0;

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub recursive: bool,
    pub include_hidden: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            recursive: true,
            include_hidden: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum LivePhoto {
    No,
    /// Это видео Live Photo; рядом лежит фото.
    VideoOf(PathBuf),
    /// Это фото Live Photo; рядом лежит видео.
    PhotoWith(PathBuf),
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaItem {
    pub path: PathBuf,
    /// Корень, который добавил пользователь (для сохранения структуры подпапок).
    pub root: PathBuf,
    pub kind: MediaKind,
    pub size: u64,
    pub video: Option<VideoInfo>,
    /// Размер фото в пикселях (для видео — в `video`).
    pub dimensions: Option<(u32, u32)>,
    pub probe_error: Option<String>,
    pub live_photo: LivePhoto,
}

/// Канонический путь без префикса `\\?\` на Windows — для сравнения дублей в очереди.
pub fn canonical(path: &Path) -> PathBuf {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC\\")
        {
            return PathBuf::from(rest);
        }
    }
    p
}

/// Находит медиафайлы. Символические ссылки не раскрываются (FR-1.2).
pub fn scan(inputs: &[PathBuf], opts: &ScanOptions, cancel: &AtomicBool) -> Vec<MediaItem> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for input in inputs {
        let root = canonical(input);
        let walker = WalkDir::new(&root)
            .follow_links(false)
            .max_depth(if opts.recursive { usize::MAX } else { 1 })
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0
                    || opts.include_hidden
                    || e.metadata().map_or(true, |m| !sys::is_hidden(e.path(), &m))
            });
        for entry in walker.flatten() {
            if cancel.load(Ordering::Relaxed) {
                return items;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path().to_path_buf();
            if !seen.insert(path.clone()) {
                continue;
            }
            let Some(kind) = media::detect(&path) else {
                continue;
            };
            let size = entry.metadata().map_or(0, |m| m.len());
            let root = if root.is_file() {
                root.parent().map(Path::to_path_buf).unwrap_or_default()
            } else {
                root.clone()
            };
            items.push(MediaItem {
                path,
                root,
                kind,
                size,
                video: None,
                dimensions: None,
                probe_error: None,
                live_photo: LivePhoto::No,
            });
        }
    }
    items
}

enum Inspected {
    Video(crate::Result<VideoInfo>),
    Image(Option<(u32, u32)>),
}

/// Пробует видео и читает размеры фото параллельно в `threads` потоков,
/// затем размечает Live Photo.
pub fn inspect(tools: &Tools, items: &mut [MediaItem], threads: usize, cancel: &AtomicBool) {
    let todo: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| match i.kind {
            MediaKind::Video => i.video.is_none() && i.probe_error.is_none(),
            MediaKind::Image(_) => i.dimensions.is_none(),
        })
        .map(|(n, _)| n)
        .collect();
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(todo.len()));
    std::thread::scope(|s| {
        for _ in 0..threads.max(1).min(todo.len().max(1)) {
            s.spawn(|| {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let n = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&idx) = todo.get(n) else { return };
                    let item = &items[idx];
                    let r = match item.kind {
                        MediaKind::Video => Inspected::Video(probe::probe(tools, &item.path)),
                        MediaKind::Image(kind) => {
                            Inspected::Image(crate::photo::dimensions(&item.path, kind))
                        }
                    };
                    results.lock().unwrap().push((idx, r));
                }
            });
        }
    });
    for (idx, r) in results.into_inner().unwrap() {
        match r {
            Inspected::Video(Ok(info)) => items[idx].video = Some(info),
            Inspected::Video(Err(e)) => items[idx].probe_error = Some(e.to_string()),
            Inspected::Image(dims) => items[idx].dimensions = dims,
        }
    }
    mark_live_photos(items);
}

/// Пара «фото + короткое MOV» с одинаковым именем в одной папке.
pub fn mark_live_photos(items: &mut [MediaItem]) {
    let key = |p: &Path| {
        (
            p.parent().map(Path::to_path_buf),
            p.file_stem().map(|s| s.to_string_lossy().to_lowercase()),
        )
    };
    let photos: HashMap<_, PathBuf> = items
        .iter()
        .filter(|i| i.kind.is_image())
        .map(|i| (key(&i.path), i.path.clone()))
        .collect();
    let mut pairs = Vec::new();
    for item in items.iter_mut() {
        let short = item
            .video
            .as_ref()
            .and_then(|v| v.duration)
            .is_some_and(|d| d <= LIVE_PHOTO_MAX_SECS);
        let is_mov = item
            .path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mov"));
        if item.kind == MediaKind::Video
            && short
            && is_mov
            && let Some(photo) = photos.get(&key(&item.path))
        {
            item.live_photo = LivePhoto::VideoOf(photo.clone());
            pairs.push((photo.clone(), item.path.clone()));
        }
    }
    for (photo, video) in pairs {
        if let Some(p) = items.iter_mut().find(|i| i.path == photo) {
            p.live_photo = LivePhoto::PhotoWith(video);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ImageKind;

    fn item(path: &str, kind: MediaKind, duration: Option<f64>) -> MediaItem {
        MediaItem {
            path: PathBuf::from(path),
            root: PathBuf::from("/"),
            kind,
            size: 0,
            video: duration.map(|d| VideoInfo {
                format_name: "mov".into(),
                duration: Some(d),
                size: 0,
                streams: vec![],
                tags: Default::default(),
            }),
            dimensions: None,
            probe_error: None,
            live_photo: LivePhoto::No,
        }
    }

    #[test]
    fn pairs_live_photos_by_stem_and_duration() {
        let mut items = vec![
            item("/d/IMG_1.HEIC", MediaKind::Image(ImageKind::Heif), None),
            item("/d/IMG_1.MOV", MediaKind::Video, Some(2.9)),
            item("/d/IMG_2.HEIC", MediaKind::Image(ImageKind::Heif), None),
            item("/d/IMG_2.MOV", MediaKind::Video, Some(30.0)),
            item("/other/IMG_1.MOV", MediaKind::Video, Some(2.0)),
        ];
        mark_live_photos(&mut items);
        assert_eq!(
            items[0].live_photo,
            LivePhoto::PhotoWith("/d/IMG_1.MOV".into())
        );
        assert_eq!(
            items[1].live_photo,
            LivePhoto::VideoOf("/d/IMG_1.HEIC".into())
        );
        assert_eq!(
            items[3].live_photo,
            LivePhoto::No,
            "длинное видео — не Live Photo"
        );
        assert_eq!(items[4].live_photo, LivePhoto::No, "другая папка");
    }

    #[test]
    fn scan_skips_hidden_and_dedupes_overlapping_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0, 0];
        std::fs::write(sub.join("a.jpg"), jpeg).unwrap();
        std::fs::write(dir.path().join(".hidden.jpg"), jpeg).unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"hello").unwrap();
        let cancel = AtomicBool::new(false);
        let found = scan(
            &[dir.path().to_path_buf(), sub.clone()],
            &ScanOptions::default(),
            &cancel,
        );
        let names: Vec<_> = found
            .iter()
            .map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        #[cfg(unix)]
        assert_eq!(names, ["a.jpg"]);
        assert_eq!(found[0].root, canonical(dir.path()));
    }
}
