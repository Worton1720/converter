use std::path::Path;

use ::image::{DynamicImage, ImageDecoder, ImageReader};
use libheif_rs::{ColorSpace, DecodingOptions, HeifContext, ImageHandle, LibHeif, RgbChroma};

use super::{Decoded, MAX_PIXELS, Pixels};
use crate::error::{Error, IoContext, Result};
use crate::exif;
use crate::media::ImageKind;

pub fn decode(path: &Path, kind: ImageKind, want_16: bool, threads: u32) -> Result<Decoded> {
    match kind {
        ImageKind::Heif => decode_heif(path, want_16, threads),
        _ => decode_generic(path, want_16),
    }
}

/// Дата съёмки для шаблона `{date}`, без декодирования пикселей.
pub fn read_date_taken(path: &Path, kind: ImageKind) -> Option<String> {
    let tiff = match kind {
        ImageKind::Heif => {
            // Файл читается в память: libheif на Windows не открывает пути не в ANSI-кодировке.
            let bytes = std::fs::read(path).ok()?;
            let ctx = HeifContext::read_from_bytes(&bytes).ok()?;
            heif_metadata(&ctx.primary_image_handle().ok()?).0?
        }
        _ => {
            let mut decoder = ImageReader::open(path)
                .ok()?
                .with_guessed_format()
                .ok()?
                .into_decoder()
                .ok()?;
            exif::normalize(&decoder.exif_metadata().ok()??)?
        }
    };
    exif::date_time_original(&tiff)
}

/// Размер в пикселях с учётом поворота — для списка файлов, без декодирования.
pub fn dimensions(path: &Path, kind: ImageKind) -> Option<(u32, u32)> {
    match kind {
        ImageKind::Heif => {
            // На Windows libheif не открывает пути вне ANSI-кодировки, поэтому читаем в память.
            #[cfg(windows)]
            let bytes = std::fs::read(path).ok()?;
            #[cfg(windows)]
            let ctx = HeifContext::read_from_bytes(&bytes).ok()?;
            #[cfg(not(windows))]
            let ctx = HeifContext::read_from_file(path.to_str()?).ok()?;
            let h = ctx.primary_image_handle().ok()?;
            Some((h.width(), h.height()))
        }
        _ => {
            let mut decoder = ImageReader::open(path)
                .ok()?
                .with_guessed_format()
                .ok()?
                .into_decoder()
                .ok()?;
            let (w, h) = decoder.dimensions();
            let swap = matches!(
                decoder.orientation().ok()?,
                ::image::metadata::Orientation::Rotate90
                    | ::image::metadata::Orientation::Rotate270
                    | ::image::metadata::Orientation::Rotate90FlipH
                    | ::image::metadata::Orientation::Rotate270FlipH
            );
            Some(if swap { (h, w) } else { (w, h) })
        }
    }
}

fn heif_err(e: libheif_rs::HeifError) -> Error {
    Error::Decode(e.to_string())
}

/// EXIF (нормализованный) и XMP основного изображения.
fn heif_metadata(handle: &ImageHandle) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut exif_block = None;
    let mut xmp = None;
    for m in handle.all_metadata() {
        if &m.item_type.0 == b"Exif" && exif_block.is_none() {
            exif_block = exif::normalize(&m.raw_data);
        } else if m.content_type == "application/rdf+xml" && xmp.is_none() {
            xmp = Some(m.raw_data);
        }
    }
    (exif_block, xmp)
}

fn decode_heif(path: &Path, want_16: bool, threads: u32) -> Result<Decoded> {
    let bytes = std::fs::read(path).at(path)?;
    let mut ctx = HeifContext::read_from_bytes(&bytes).map_err(heif_err)?;
    ctx.set_max_decoding_threads(threads.max(1));
    let handle = ctx.primary_image_handle().map_err(heif_err)?;
    let (w, h) = (handle.width(), handle.height());
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(Error::Decode(format!(
            "слишком большое изображение {w}×{h}"
        )));
    }

    let alpha = handle.has_alpha_channel();
    let source_bits = handle.luma_bits_per_pixel();
    let hdr = want_16 && source_bits > 8;
    let chroma = match (hdr, alpha) {
        (false, false) => RgbChroma::Rgb,
        (false, true) => RgbChroma::Rgba,
        (true, false) => RgbChroma::HdrRgbLe,
        (true, true) => RgbChroma::HdrRgbaLe,
    };
    let mut options = DecodingOptions::new()
        .ok_or_else(|| Error::Decode("libheif: нет памяти под параметры".into()))?;
    options.set_convert_hdr_to_8bit(!hdr);

    let lib = LibHeif::new();
    // Преобразования контейнера (irot/imir/clap) применяются здесь — источник истины для HEIC.
    let image = lib
        .decode(&handle, ColorSpace::Rgb(chroma), Some(options))
        .map_err(heif_err)?;
    let planes = image.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| Error::Decode("libheif не вернул RGB-плоскость".into()))?;
    let (pw, ph) = (plane.width as usize, plane.height as usize);
    let channels = if alpha { 4 } else { 3 };

    let pixels = if hdr {
        let bits = plane.bits_per_pixel as u32;
        let row = pw * channels * 2;
        let mut out = Vec::with_capacity(pw * ph * channels);
        for y in 0..ph {
            let line = &plane.data[y * plane.stride..y * plane.stride + row];
            out.extend(line.as_chunks::<2>().0.iter().map(|b| {
                let v = u16::from_le_bytes([b[0], b[1]]) as u32;
                // Растягиваем 10/12 бит на полный 16-битный диапазон.
                ((v << (16 - bits)) | (v >> (2 * bits).saturating_sub(16))) as u16
            }));
        }
        if alpha {
            Pixels::Rgba16(out)
        } else {
            Pixels::Rgb16(out)
        }
    } else {
        let row = pw * channels;
        let mut out = Vec::with_capacity(row * ph);
        for y in 0..ph {
            out.extend_from_slice(&plane.data[y * plane.stride..y * plane.stride + row]);
        }
        if alpha {
            Pixels::Rgba8(out)
        } else {
            Pixels::Rgb8(out)
        }
    };

    let icc = handle.color_profile_raw().map(|p| p.data);
    let (exif, xmp) = heif_metadata(&handle);
    Ok(Decoded {
        width: pw as u32,
        height: ph as u32,
        pixels,
        icc,
        exif,
        xmp,
        source_bits,
    })
}

fn image_err(e: ::image::ImageError) -> Error {
    Error::Decode(e.to_string())
}

fn decode_generic(path: &Path, want_16: bool) -> Result<Decoded> {
    let reader = ImageReader::open(path)
        .at(path)?
        .with_guessed_format()
        .at(path)?;
    let mut decoder = reader.into_decoder().map_err(image_err)?;
    let (w, h) = decoder.dimensions();
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(Error::Decode(format!(
            "слишком большое изображение {w}×{h}"
        )));
    }
    let icc = decoder.icc_profile().ok().flatten();
    let exif = decoder
        .exif_metadata()
        .ok()
        .flatten()
        .and_then(|raw| exif::normalize(&raw));
    let xmp = decoder.xmp_metadata().ok().flatten();
    let orientation = decoder
        .orientation()
        .unwrap_or(::image::metadata::Orientation::NoTransforms);
    let source_bits =
        (decoder.color_type().bits_per_pixel() / decoder.color_type().channel_count() as u16) as u8;

    let mut img = DynamicImage::from_decoder(decoder).map_err(image_err)?;
    img.apply_orientation(orientation);
    let (width, height) = (img.width(), img.height());
    let alpha = img.color().has_alpha();
    let pixels = match (want_16 && source_bits > 8, alpha) {
        (false, false) => Pixels::Rgb8(img.into_rgb8().into_raw()),
        (false, true) => Pixels::Rgba8(img.into_rgba8().into_raw()),
        (true, false) => Pixels::Rgb16(img.into_rgb16().into_raw()),
        (true, true) => Pixels::Rgba16(img.into_rgba16().into_raw()),
    };
    Ok(Decoded {
        width,
        height,
        pixels,
        icc,
        exif,
        xmp,
        source_bits,
    })
}
