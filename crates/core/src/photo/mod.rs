//! Конвертация фото (раздел 4.3 ТЗ): декодирование → поворот/масштаб/цвет → кодирование.

mod color;
mod decode;
mod encode;
mod webp_mux;

use std::path::Path;

use crate::error::{Error, Result};
use crate::exif;
use crate::media::ImageKind;
use crate::options::{ImageFormat, ImageOptions, MetadataPolicy};

pub use decode::{decode, dimensions, read_date_taken};

/// Предел WebP по стороне (FR-2.6).
pub const WEBP_MAX_SIDE: u32 = 16_383;
/// Предел JPEG по стороне.
pub const JPEG_MAX_SIDE: u32 = 65_535;
/// Защита от «бомб» с огромным разрешением (раздел 8.2).
pub const MAX_PIXELS: u64 = 400_000_000;

#[derive(Debug, Clone)]
pub enum Pixels {
    Rgb8(Vec<u8>),
    Rgba8(Vec<u8>),
    /// 16 бит на канал, полный диапазон 0–65535.
    Rgb16(Vec<u16>),
    Rgba16(Vec<u16>),
}

impl Pixels {
    pub fn has_alpha(&self) -> bool {
        matches!(self, Pixels::Rgba8(_) | Pixels::Rgba16(_))
    }

    pub fn is_16bit(&self) -> bool {
        matches!(self, Pixels::Rgb16(_) | Pixels::Rgba16(_))
    }

    pub fn channels(&self) -> usize {
        if self.has_alpha() { 4 } else { 3 }
    }

    /// Приводит к 8 битам на канал.
    pub fn into_8bit(self) -> Pixels {
        let down = |v: Vec<u16>| {
            v.into_iter()
                .map(|x| ((x as u32 + 128) / 257) as u8)
                .collect()
        };
        match self {
            Pixels::Rgb16(v) => Pixels::Rgb8(down(v)),
            Pixels::Rgba16(v) => Pixels::Rgba8(down(v)),
            p => p,
        }
    }

    /// Убирает альфу, накладывая изображение на белый фон (для JPEG).
    pub fn flatten_alpha(self) -> Pixels {
        match self.into_8bit() {
            Pixels::Rgba8(v) => Pixels::Rgb8(
                v.as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| {
                        let a = p[3] as u32;
                        let mix = move |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
                        [mix(p[0]), mix(p[1]), mix(p[2])]
                    })
                    .collect(),
            ),
            p => p,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
    pub icc: Option<Vec<u8>>,
    /// EXIF как «голый» TIFF (см. [`exif::normalize`]).
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    /// Глубина исходника (8, 10, 12, 16).
    pub source_bits: u8,
}

#[derive(Debug, Clone)]
pub struct Converted {
    pub width: u32,
    pub height: u32,
    pub operation: String,
}

/// Совпадает ли выходной формат с входным без каких-либо изменений (раздел 4.2).
pub fn is_noop(kind: ImageKind, opts: &ImageOptions, metadata: MetadataPolicy) -> bool {
    let same_format = matches!(
        (kind, opts.format),
        (ImageKind::Png, ImageFormat::Png)
            | (ImageKind::Jpeg, ImageFormat::Jpeg)
            | (ImageKind::Webp, ImageFormat::Webp)
    );
    same_format && opts.max_side.is_none() && !opts.to_srgb && metadata == MetadataPolicy::Keep
}

pub fn convert(
    source: &Path,
    kind: ImageKind,
    dest: &Path,
    opts: &ImageOptions,
    metadata: MetadataPolicy,
    threads: u32,
) -> Result<Converted> {
    let want_16 = opts.format == ImageFormat::Png && opts.png_16bit;
    let mut img = decode(source, kind, want_16, threads)?;

    let mut target_side = opts.max_side;
    let longest = img.width.max(img.height);
    match opts.format {
        ImageFormat::Webp if longest > WEBP_MAX_SIDE => {
            if !opts.fit_webp_limit {
                return Err(Error::TooLarge {
                    width: img.width,
                    height: img.height,
                    format: "WebP",
                    max: WEBP_MAX_SIDE,
                });
            }
            target_side = Some(target_side.map_or(WEBP_MAX_SIDE, |s| s.min(WEBP_MAX_SIDE)));
        }
        ImageFormat::Jpeg if longest > JPEG_MAX_SIDE => {
            return Err(Error::TooLarge {
                width: img.width,
                height: img.height,
                format: "JPEG",
                max: JPEG_MAX_SIDE,
            });
        }
        _ => {}
    }
    if let Some(side) = target_side.filter(|&s| s < longest) {
        img = color::resize(img, side)?;
    }
    if opts.to_srgb {
        img = color::to_srgb(img)?;
    }
    prepare_metadata(&mut img, metadata);

    match opts.format {
        ImageFormat::Png => encode::png(&img, dest, opts.png_compression)?,
        ImageFormat::Jpeg => {
            img.pixels =
                std::mem::replace(&mut img.pixels, Pixels::Rgb8(Vec::new())).flatten_alpha();
            add_exif_thumbnail(&mut img);
            encode::jpeg(&img, dest, opts.quality)?
        }
        ImageFormat::Webp => {
            img.pixels = std::mem::replace(&mut img.pixels, Pixels::Rgb8(Vec::new())).into_8bit();
            encode::webp(&img, dest, opts.quality, threads)?
        }
    }
    Ok(Converted {
        width: img.width,
        height: img.height,
        operation: format!("{} → {}", kind.label(), opts.format.label()),
    })
}

/// Поворот уже применён к пикселям, поэтому Orientation = 1 (FR-2.3).
/// Если блок не разобрать, его безопаснее выбросить, чем получить двойной поворот или утечку GPS.
fn prepare_metadata(img: &mut Decoded, policy: MetadataPolicy) {
    if policy == MetadataPolicy::StripAll {
        img.exif = None;
        img.xmp = None;
        return;
    }
    if let Some(tiff) = img.exif.as_mut() {
        let ok = exif::reset_orientation(tiff)
            && (policy != MetadataPolicy::StripGps || exif::strip_gps(tiff));
        if !ok {
            img.exif = None;
        }
    }
    if policy == MetadataPolicy::StripGps {
        // В XMP тоже бывают координаты (exif:GPSLatitude); править XML на месте ненадёжно.
        img.xmp = None;
    }
}

/// Сторона EXIF-миниатюры: как у камер и телефонов.
const EXIF_THUMB_SIDE: u32 = 160;

/// EXIF-миниатюра в JPEG по уже повёрнутой картинке: по ней поиск похожих фото потом не
/// декодирует снимок целиком (FR-9.1). Только если EXIF есть: при «удалить все метаданные»
/// миниатюра не добавляется; если блок с ней не влезет в APP1, остаётся без неё.
fn add_exif_thumbnail(img: &mut Decoded) {
    let (Some(tiff), Pixels::Rgb8(data)) = (img.exif.as_ref(), &img.pixels) else {
        return;
    };
    let Some(view) = ::image::ImageBuffer::<::image::Rgb<u8>, &[u8]>::from_raw(
        img.width,
        img.height,
        data.as_slice(),
    ) else {
        return;
    };
    let scale = f64::from(EXIF_THUMB_SIDE) / f64::from(img.width.max(img.height));
    let (w, h) = if scale >= 1.0 {
        return; // картинка и так маленькая — миниатюра не нужна
    } else {
        (
            ((f64::from(img.width) * scale).round() as u32).max(1),
            ((f64::from(img.height) * scale).round() as u32).max(1),
        )
    };
    let thumb = ::image::imageops::thumbnail(&view, w, h);
    let mut jpeg = Vec::new();
    if ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 75)
        .encode_image(&thumb)
        .is_err()
    {
        return;
    }
    if let Some(with) = exif::with_thumbnail(tiff, &jpeg)
        && with.len() + 6 <= encode::JPEG_APP1_MAX
    {
        img.exif = Some(with);
    }
}

/// Проверка результата (FR-5.2). `full` — полное декодирование, а не только заголовок.
pub fn verify(path: &Path, width: u32, height: u32, full: bool) -> Result<()> {
    let (w, h) = if full {
        let img = ::image::ImageReader::open(path)
            .and_then(|r| r.with_guessed_format())
            .map_err(|e| Error::Verify(e.to_string()))?
            .decode()
            .map_err(|e| Error::Verify(e.to_string()))?;
        (img.width(), img.height())
    } else {
        ::image::image_dimensions(path).map_err(|e| Error::Verify(e.to_string()))?
    };
    if (w, h) != (width, height) {
        return Err(Error::Verify(format!(
            "размер {w}×{h}, ожидался {width}×{height}"
        )));
    }
    Ok(())
}
