//! Миниатюры для списка и дубликатов с кэшем на диске (раздел 9 ТЗ).

use std::path::{Path, PathBuf};

use ::image::{DynamicImage, ImageDecoder, ImageReader, RgbImage, RgbaImage};
use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

use crate::error::{Error, IoContext, Result};
use crate::exif;
use crate::ffmpeg::{self, Tools};
use crate::media::{ImageKind, MediaKind};
use crate::paths;

fn cache_file(path: &Path, size: u32) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let mut h = blake3::Hasher::new();
    h.update(path.to_string_lossy().as_bytes());
    h.update(&meta.len().to_le_bytes());
    h.update(&mtime.to_le_bytes());
    h.update(&size.to_le_bytes());
    let hex = h.finalize().to_hex();
    Some(
        paths::cache_dir()
            .join("thumbs")
            .join(&hex[..2])
            .join(format!("{}.jpg", &hex[2..32])),
    )
}

/// Путь к JPEG-миниатюре (большая сторона ≤ `size`). Создаёт её при первом запросе.
pub fn thumbnail(tools: &Tools, path: &Path, kind: MediaKind, size: u32) -> Result<PathBuf> {
    let out = cache_file(path, size).ok_or_else(|| Error::Invalid("файл недоступен".into()))?;
    if out.exists() {
        return Ok(out);
    }
    let dir = out.parent().expect("у файла кэша есть папка");
    std::fs::create_dir_all(dir).at(dir)?;
    let tmp = out.with_extension(format!("{}.tmp", std::process::id()));
    match kind {
        MediaKind::Video => video_frame(tools, path, size, &tmp)?,
        MediaKind::Image(kind) => {
            preview(path, kind, size)?
                .thumbnail(size, size)
                .to_rgb8()
                .save_with_format(&tmp, ::image::ImageFormat::Jpeg)
                .map_err(|e| Error::Encode(e.to_string()))?;
        }
    }
    std::fs::rename(&tmp, &out).at(&out)?;
    Ok(out)
}

/// Уменьшенная картинка с применённым поворотом, без кэша — для сравнения фото (FR-9.1, 9.2).
/// HEIC и JPEG — по встроенной миниатюре, если её хватает: полное декодирование
/// 24 Мп занимает 0,1–0,7 с, а миниатюра читается за миллисекунды.
pub(crate) fn preview(path: &Path, kind: ImageKind, size: u32) -> Result<DynamicImage> {
    match kind {
        ImageKind::Heif => heif_preview(path, size),
        ImageKind::Jpeg if size <= EXIF_THUMB_SIDE => match exif_preview(path) {
            Some(img) => Ok(img),
            None => generic_preview(path),
        },
        _ => generic_preview(path),
    }
}

/// Сторона EXIF-миниатюры JPEG (обычно 160×120): для превью больше неё она не годится.
const EXIF_THUMB_SIDE: u32 = 160;

/// Миниатюра из EXIF JPEG с поворотом основного изображения. `None`, если её нет или её
/// пропорции не совпадают со снимком: после поворота или обрезки в редакторе она бывает
/// устаревшей, и сравнение по ней дало бы чужой отпечаток.
fn exif_preview(path: &Path) -> Option<DynamicImage> {
    let mut decoder = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let (w, h) = decoder.dimensions();
    let orientation = decoder
        .orientation()
        .unwrap_or(::image::metadata::Orientation::NoTransforms);
    let tiff = exif::normalize(&decoder.exif_metadata().ok()??)?;
    let mut thumb =
        ::image::load_from_memory_with_format(exif::thumbnail(&tiff)?, ::image::ImageFormat::Jpeg)
            .ok()?;
    let (tw, th) = (thumb.width(), thumb.height());
    let (a, b) = (
        f64::from(w) / f64::from(h.max(1)),
        f64::from(tw) / f64::from(th.max(1)),
    );
    if (a - b).abs() > 0.03 * a {
        return None;
    }
    thumb.apply_orientation(orientation);
    Some(thumb)
}

/// Встроенная миниатюра HEIC, если её хватает по размеру; иначе основное изображение.
fn heif_preview(path: &Path, size: u32) -> Result<DynamicImage> {
    let err = |e: libheif_rs::HeifError| Error::Decode(e.to_string());
    let bytes = std::fs::read(path).at(path)?;
    let mut ctx = HeifContext::read_from_bytes(&bytes).map_err(err)?;
    ctx.set_max_decoding_threads(2);
    let primary = ctx.primary_image_handle().map_err(err)?;
    let mut ids = vec![0; primary.number_of_thumbnails()];
    primary.thumbnail_ids(&mut ids);
    let handle = ids
        .iter()
        .filter_map(|&id| primary.thumbnail(id).ok())
        .find(|t| t.width().max(t.height()) >= size / 2)
        .unwrap_or(primary);
    let alpha = handle.has_alpha_channel();
    let chroma = if alpha {
        RgbChroma::Rgba
    } else {
        RgbChroma::Rgb
    };
    let image = LibHeif::new()
        .decode(&handle, ColorSpace::Rgb(chroma), None)
        .map_err(err)?;
    let planes = image.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| Error::Decode("нет RGB-плоскости".into()))?;
    let channels = if alpha { 4 } else { 3 };
    let row = plane.width as usize * channels;
    let mut data = Vec::with_capacity(row * plane.height as usize);
    for y in 0..plane.height as usize {
        data.extend_from_slice(&plane.data[y * plane.stride..y * plane.stride + row]);
    }
    let bad = || Error::Decode("неверный размер буфера".into());
    Ok(if alpha {
        DynamicImage::ImageRgba8(
            RgbaImage::from_raw(plane.width, plane.height, data).ok_or_else(bad)?,
        )
    } else {
        DynamicImage::ImageRgb8(
            RgbImage::from_raw(plane.width, plane.height, data).ok_or_else(bad)?,
        )
    })
}

fn generic_preview(path: &Path) -> Result<DynamicImage> {
    let mut decoder = ImageReader::open(path)
        .at(path)?
        .with_guessed_format()
        .at(path)?
        .into_decoder()
        .map_err(|e| Error::Decode(e.to_string()))?;
    let orientation = decoder
        .orientation()
        .unwrap_or(::image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| Error::Decode(e.to_string()))?;
    img.apply_orientation(orientation);
    Ok(img)
}

fn video_frame(tools: &Tools, path: &Path, size: u32, out: &Path) -> Result<()> {
    let mut cmd = tools.ffmpeg_cmd();
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "1", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={size}:{size}:force_original_aspect_ratio=decrease"),
            "-q:v",
            "4",
            "-f",
            "image2",
        ])
        .arg(out);
    let first = ffmpeg::output(&mut cmd).map_err(|e| Error::Probe(e.to_string()))?;
    if first.status.success() && out.exists() {
        return Ok(());
    }
    // Видео короче секунды — берём первый кадр.
    let mut cmd = tools.ffmpeg_cmd();
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={size}:{size}:force_original_aspect_ratio=decrease"),
            "-f",
            "image2",
        ])
        .arg(out);
    let second = ffmpeg::output(&mut cmd).map_err(|e| Error::Probe(e.to_string()))?;
    if second.status.success() {
        Ok(())
    } else {
        Err(Error::Probe(
            String::from_utf8_lossy(&second.stderr).trim().to_string(),
        ))
    }
}

/// Удаляет кэш миниатюр. Возвращает освобождённые байты.
pub fn clear_cache() -> u64 {
    let dir = paths::cache_dir().join("thumbs");
    let size = walkdir::WalkDir::new(&dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum();
    let _ = std::fs::remove_dir_all(dir);
    size
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JPEG `w`×`h` со встроенной EXIF-миниатюрой `tw`×`th`.
    fn jpeg_with_thumb(dir: &Path, (w, h): (u32, u32), (tw, th): (u32, u32)) -> PathBuf {
        let encode = |w, h| {
            let mut out = std::io::Cursor::new(Vec::new());
            DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, _| {
                ::image::Rgb([x as u8, 90, 160])
            }))
            .write_to(&mut out, ::image::ImageFormat::Jpeg)
            .unwrap();
            out.into_inner()
        };
        let thumb = encode(tw, th);
        // TIFF: пустой IFD0 @8 → IFD1 @14 {смещение, длина миниатюры} → миниатюра @44.
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes());
        tiff.extend_from_slice(&14u32.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        for (tag, value) in [(0x0201u16, 44u32), (0x0202, thumb.len() as u32)] {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&4u16.to_le_bytes());
            tiff.extend_from_slice(&1u32.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&thumb);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let main = encode(w, h);
        let mut file = main[..2].to_vec();
        file.extend_from_slice(&[0xFF, 0xE1]);
        file.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        file.extend_from_slice(&app1);
        file.extend_from_slice(&main[2..]);
        let path = dir.join(format!("{w}x{h}-{tw}x{th}.jpg"));
        std::fs::write(&path, file).unwrap();
        path
    }

    #[test]
    fn jpeg_preview_uses_exif_thumbnail_only_when_it_fits() {
        let dir = tempfile::tempdir().unwrap();
        let good = jpeg_with_thumb(dir.path(), (800, 600), (160, 120));
        let img = preview(&good, ImageKind::Jpeg, 64).unwrap();
        assert_eq!((img.width(), img.height()), (160, 120), "взята миниатюра");
        // Для большого превью миниатюра мала — декодируется само фото.
        let img = preview(&good, ImageKind::Jpeg, 400).unwrap();
        assert_eq!((img.width(), img.height()), (800, 600));
        // Миниатюра не той ориентации (фото повернули, её — нет) — не используется.
        let stale = jpeg_with_thumb(dir.path(), (800, 600), (120, 160));
        assert!(exif_preview(&stale).is_none());
        let img = preview(&stale, ImageKind::Jpeg, 64).unwrap();
        assert_eq!((img.width(), img.height()), (800, 600));
    }
}
