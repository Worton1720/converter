//! Параметры конвертации (разделы 4.3–4.6 ТЗ).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
}

impl ImageFormat {
    pub fn label(self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Webp => "WebP",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PngCompression {
    #[default]
    Fast,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MetadataPolicy {
    #[default]
    Keep,
    StripGps,
    StripAll,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageOptions {
    pub format: ImageFormat,
    /// Качество JPEG/WebP, 1–100.
    pub quality: u8,
    pub png_compression: PngCompression,
    /// Уменьшать по большей стороне до N px.
    pub max_side: Option<u32>,
    /// Сохранять PNG в 16 бит, если источник глубже 8 бит.
    pub png_16bit: bool,
    /// Преобразовывать цвета в sRGB вместо переноса ICC-профиля.
    pub to_srgb: bool,
    /// Уменьшать изображения, превышающие предел WebP (FR-2.6).
    pub fit_webp_limit: bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            format: ImageFormat::Jpeg,
            quality: 90,
            png_compression: PngCompression::Fast,
            max_side: None,
            png_16bit: false,
            to_srgb: false,
            fit_webp_limit: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebmCodec {
    Av1,
    Vp9,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoMode {
    /// H.264 8 бит + AAC в MP4.
    Compat,
    /// HEVC + AAC в MP4 (тег hvc1).
    Compact,
    Webm(WebmCodec),
    /// Копирование потоков в MKV.
    Mkv,
}

impl VideoMode {
    pub fn extension(self) -> &'static str {
        match self {
            VideoMode::Compat | VideoMode::Compact => "mp4",
            VideoMode::Webm(_) => "webm",
            VideoMode::Mkv => "mkv",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QualityProfile {
    Fast,
    #[default]
    Balanced,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HwBackend {
    Nvenc,
    Qsv,
    Amf,
    Vaapi,
}

impl HwBackend {
    pub fn name(self) -> &'static str {
        match self {
            HwBackend::Nvenc => "NVENC",
            HwBackend::Qsv => "QSV",
            HwBackend::Amf => "AMF",
            HwBackend::Vaapi => "VAAPI",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HwPreference {
    #[default]
    Auto,
    Off,
    Only(HwBackend),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoOptions {
    pub mode: VideoMode,
    pub quality: QualityProfile,
    pub hw: HwPreference,
    pub audio_bitrate_kbps: u32,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            mode: VideoMode::Compat,
            quality: QualityProfile::Balanced,
            hw: HwPreference::Auto,
            audio_bitrate_kbps: 192,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OutputDir {
    #[default]
    BesideSource,
    Into(PathBuf),
    /// В папку с сохранением структуры подпапок относительно корня сканирования.
    IntoKeepTree(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ConflictPolicy {
    #[default]
    Suffix,
    Overwrite,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SourcePolicy {
    #[default]
    Keep,
    Trash,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvertOptions {
    pub image: ImageOptions,
    pub video: VideoOptions,
    pub metadata: MetadataPolicy,
    pub output: OutputDir,
    /// Шаблон имени: `{name}`, `{date}`, `{ext}`, `{n}`.
    pub name_template: String,
    pub conflict: ConflictPolicy,
    pub sources: SourcePolicy,
    pub preserve_mtime: bool,
    /// Пропускать видео Live Photo (FR-1.7).
    pub skip_live_photo_video: bool,
    /// Удалять пропущенное видео Live Photo вместе с фото (FR-5.5).
    pub remove_live_photo_video: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            image: ImageOptions::default(),
            video: VideoOptions::default(),
            metadata: MetadataPolicy::Keep,
            output: OutputDir::BesideSource,
            name_template: "{name}".into(),
            conflict: ConflictPolicy::Suffix,
            sources: SourcePolicy::Keep,
            preserve_mtime: true,
            skip_live_photo_video: true,
            remove_live_photo_video: false,
        }
    }
}
