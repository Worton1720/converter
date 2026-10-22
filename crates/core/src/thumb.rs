//! Миниатюры для списка и дубликатов с кэшем на диске (раздел 9 ТЗ).

use std::path::{Path, PathBuf};

use ::image::{DynamicImage, ImageDecoder, ImageReader, RgbImage, RgbaImage};
use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

use crate::error::{Error, IoContext, Result};
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
            let img = match kind {
                ImageKind::Heif => heif_preview(path, size)?,
                _ => generic_preview(path)?,
            };
            img.thumbnail(size, size)
                .to_rgb8()
                .save_with_format(&tmp, ::image::ImageFormat::Jpeg)
                .map_err(|e| Error::Encode(e.to_string()))?;
        }
    }
    std::fs::rename(&tmp, &out).at(&out)?;
    Ok(out)
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
