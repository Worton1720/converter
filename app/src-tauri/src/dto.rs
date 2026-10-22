//! Структуры, которые уходят в интерфейс. Отдельно от ядра, чтобы фронтенду
//! не приходилось разбирать внутренние типы (VideoInfo со всеми потоками и т.п.).

use serde::Serialize;

use converter_core::dupes::{self, DupesReport, GroupKind, KeepRule};
use converter_core::governor::GovernorStatus;
use converter_core::media::{ImageKind, MediaKind};
use converter_core::scan::{LivePhoto, MediaItem};
use converter_core::settings::Settings;
use converter_core::video::HwEncoder;

#[derive(Serialize)]
pub struct FfmpegInfo {
    pub path: String,
    pub version: String,
    pub origin: String,
}

#[derive(Serialize)]
pub struct HwDto {
    pub backend: &'static str,
    pub codec: String,
    pub device: Option<String>,
}

pub fn hw_encoder(e: &HwEncoder) -> HwDto {
    HwDto {
        backend: e.backend.name(),
        codec: format!("{:?}", e.codec),
        device: e.device.clone(),
    }
}

#[derive(Serialize)]
pub struct AppInfo {
    pub version: String,
    pub threads: usize,
    pub ffmpeg: Option<FfmpegInfo>,
    pub ffmpeg_error: Option<String>,
    pub hw: Vec<HwDto>,
    pub max_sessions: usize,
    pub settings: Settings,
    pub settings_path: String,
    pub log_path: String,
    pub startup_paths: Vec<String>,
    pub status: Option<GovernorStatus>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ItemDto {
    pub path: String,
    pub name: String,
    pub kind: &'static str,
    /// Короткое имя формата для колонки «Тип».
    pub format: String,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration: Option<f64>,
    pub codec: Option<String>,
    pub hdr: bool,
    /// `no` / `video` (видео Live Photo) / `photo` (фото Live Photo).
    pub live: &'static str,
    pub error: Option<String>,
    pub inspected: bool,
}

impl From<&MediaItem> for ItemDto {
    fn from(i: &MediaItem) -> Self {
        let name = i
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (kind, mut format) = match i.kind {
            MediaKind::Image(k) => (
                "image",
                match k {
                    ImageKind::Heif => "HEIC",
                    ImageKind::Jpeg => "JPEG",
                    ImageKind::Png => "PNG",
                    ImageKind::Webp => "WebP",
                    ImageKind::Bmp => "BMP",
                    ImageKind::Tiff => "TIFF",
                }
                .to_string(),
            ),
            MediaKind::Video => (
                "video",
                i.path
                    .extension()
                    .map(|e| e.to_string_lossy().to_uppercase())
                    .unwrap_or_else(|| "видео".into()),
            ),
        };
        let mut width = i.dimensions.map(|d| d.0);
        let mut height = i.dimensions.map(|d| d.1);
        let mut codec = None;
        let mut hdr = false;
        let duration = i.video.as_ref().and_then(|v| v.duration);
        if let Some(v) = i.video.as_ref().and_then(|v| v.main_video()) {
            let swap = v.rotation.abs() % 180 == 90;
            width = Some(if swap { v.height } else { v.width });
            height = Some(if swap { v.width } else { v.height });
            codec = v.codec.clone().map(|c| c.to_uppercase());
            hdr = v.is_hdr();
            if let Some(c) = &codec {
                format = if c == "H264" {
                    "H.264".into()
                } else {
                    c.clone()
                };
            }
        }
        ItemDto {
            path: i.path.to_string_lossy().into_owned(),
            name,
            kind,
            format,
            size: i.size,
            width,
            height,
            duration,
            codec,
            hdr,
            live: match i.live_photo {
                LivePhoto::No => "no",
                LivePhoto::VideoOf(_) => "video",
                LivePhoto::PhotoWith(_) => "photo",
            },
            error: i.probe_error.clone(),
            inspected: i.video.is_some() || i.dimensions.is_some() || i.probe_error.is_some(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupFileDto {
    pub path: String,
    pub size: u64,
    pub mtime_ms: f64,
    pub links: usize,
    pub kind: Option<&'static str>,
    /// Формат фото («HEIC», «JPEG»…) — в группах «одно фото в разных форматах».
    pub format: Option<&'static str>,
}

#[derive(Serialize)]
pub struct DupGroupDto {
    pub kind: GroupKind,
    pub size: u64,
    pub hash: String,
    pub files: Vec<DupFileDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupesResult {
    pub groups: Vec<DupGroupDto>,
    pub marks: Vec<Vec<bool>>,
    pub scanned: usize,
    pub skipped_cloud: usize,
    pub hardlinks: usize,
    pub errors: Vec<(String, String)>,
}

impl DupesResult {
    pub fn new(r: &DupesReport, keep: &KeepRule, keep_format: ImageKind) -> Self {
        DupesResult {
            groups: r
                .groups
                .iter()
                .map(|g| DupGroupDto {
                    kind: g.kind,
                    size: g.size,
                    hash: g.hash.clone(),
                    files: g
                        .files
                        .iter()
                        .map(|f| DupFileDto {
                            path: f.path.to_string_lossy().into_owned(),
                            size: f.size,
                            mtime_ms: (f.mtime_ns / 1_000_000) as f64,
                            links: f.links.len(),
                            kind: converter_core::media::detect(&f.path).map(|k| match k {
                                MediaKind::Image(_) => "image",
                                MediaKind::Video => "video",
                            }),
                            format: f.format.map(ImageKind::label),
                        })
                        .collect(),
                })
                .collect(),
            marks: r
                .groups
                .iter()
                .map(|g| dupes::auto_select(g, keep, keep_format))
                .collect(),
            scanned: r.scanned,
            skipped_cloud: r.skipped_cloud,
            hardlinks: r.hardlinks.len(),
            errors: r
                .errors
                .iter()
                .map(|(p, e)| (p.to_string_lossy().into_owned(), e.clone()))
                .collect(),
        }
    }
}
