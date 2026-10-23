use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use super::webp_mux;
use super::{Decoded, Pixels};
use crate::error::{Error, IoContext, Result};
use crate::options::PngCompression;

/// Предел полезной нагрузки сегмента APP1 в JPEG.
pub(super) const JPEG_APP1_MAX: usize = 65_533;
const XMP_JPEG_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

fn encode_err(e: impl std::fmt::Display) -> Error {
    Error::Encode(e.to_string())
}

fn create(dest: &Path) -> Result<BufWriter<File>> {
    Ok(BufWriter::new(File::create(dest).at(dest)?))
}

pub fn png(img: &Decoded, dest: &Path, compression: PngCompression) -> Result<()> {
    let mut info = png::Info::with_size(img.width, img.height);
    info.color_type = if img.pixels.has_alpha() {
        png::ColorType::Rgba
    } else {
        png::ColorType::Rgb
    };
    info.bit_depth = if img.pixels.is_16bit() {
        png::BitDepth::Sixteen
    } else {
        png::BitDepth::Eight
    };
    info.icc_profile = img.icc.as_deref().map(Cow::Borrowed);
    info.exif_metadata = img.exif.as_deref().map(Cow::Borrowed);

    let out = create(dest)?;
    let mut encoder = png::Encoder::with_info(out, info).map_err(encode_err)?;
    encoder.set_compression(match compression {
        PngCompression::Fast => png::Compression::Fast,
        PngCompression::Compact => png::Compression::High,
    });
    if let Some(xmp) = img.xmp.as_deref().and_then(|x| std::str::from_utf8(x).ok()) {
        encoder
            .add_itxt_chunk("XML:com.adobe.xmp".into(), xmp.to_string())
            .map_err(encode_err)?;
    }
    let mut writer = encoder.write_header().map_err(encode_err)?;
    match &img.pixels {
        Pixels::Rgb8(v) | Pixels::Rgba8(v) => writer.write_image_data(v),
        // PNG хранит 16-битные отсчёты в big-endian.
        Pixels::Rgb16(v) | Pixels::Rgba16(v) => {
            let be: Vec<u8> = v.iter().flat_map(|x| x.to_be_bytes()).collect();
            writer.write_image_data(&be)
        }
    }
    .map_err(encode_err)?;
    writer.finish().map_err(encode_err)?;
    Ok(())
}

/// JPEG через libjpeg-turbo (SIMD), 4:2:0. Метаданные — сегментами сразу после SOI и JFIF:
/// профиль цвета (APP2, частями), EXIF и XMP (APP1).
pub fn jpeg(img: &Decoded, dest: &Path, quality: u8) -> Result<()> {
    let Pixels::Rgb8(data) = &img.pixels else {
        return Err(Error::Encode("JPEG ожидает RGB 8 бит".into()));
    };
    let mut c = turbojpeg::Compressor::new().map_err(encode_err)?;
    c.set_quality(i32::from(quality.clamp(1, 100)))
        .map_err(encode_err)?;
    c.set_subsamp(turbojpeg::Subsamp::Sub2x2)
        .map_err(encode_err)?;
    let jpeg = c
        .compress_to_vec(turbojpeg::Image {
            pixels: data.as_slice(),
            width: img.width as usize,
            pitch: img.width as usize * 3,
            height: img.height as usize,
            format: turbojpeg::PixelFormat::RGB,
        })
        .map_err(encode_err)?;
    let mut segments: Vec<u8> = Vec::new();
    let mut push = |marker: u8, payload: &[u8]| {
        segments.extend_from_slice(&[0xFF, marker]);
        segments.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        segments.extend_from_slice(payload);
    };
    if let Some(icc) = &img.icc {
        // ICC в APP2 частями до 65 519 байт: «ICC_PROFILE\0», номер, всего частей.
        let chunks: Vec<&[u8]> = icc.chunks(65_519).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let mut p = b"ICC_PROFILE\0".to_vec();
            p.extend_from_slice(&[(i + 1) as u8, chunks.len() as u8]);
            p.extend_from_slice(chunk);
            push(0xE2, &p);
        }
    }
    if let Some(exif) = img.exif.as_ref().filter(|e| e.len() + 6 <= JPEG_APP1_MAX) {
        let mut p = b"Exif\0\0".to_vec();
        p.extend_from_slice(exif);
        push(0xE1, &p);
    }
    if let Some(xmp) = img
        .xmp
        .as_ref()
        .filter(|x| x.len() + XMP_JPEG_HEADER.len() <= JPEG_APP1_MAX)
    {
        let mut p = XMP_JPEG_HEADER.to_vec();
        p.extend_from_slice(xmp);
        push(0xE1, &p);
    }
    // После SOI и APP0 (JFIF), если он есть.
    let mut at = 2;
    if jpeg.get(2..4) == Some(&[0xFF, 0xE0]) {
        at += 2 + usize::from(u16::from_be_bytes([jpeg[4], jpeg[5]]));
    }
    let mut out = create(dest)?;
    out.write_all(&jpeg[..at]).at(dest)?;
    out.write_all(&segments).at(dest)?;
    out.write_all(&jpeg[at..]).at(dest)?;
    out.flush().at(dest)?;
    Ok(())
}

pub fn webp(img: &Decoded, dest: &Path, quality: u8, threads: u32) -> Result<()> {
    let encoder = match &img.pixels {
        Pixels::Rgb8(v) => webp::Encoder::from_rgb(v, img.width, img.height),
        Pixels::Rgba8(v) => webp::Encoder::from_rgba(v, img.width, img.height),
        _ => return Err(Error::Encode("WebP ожидает 8 бит".into())),
    };
    let mut config = webp::WebPConfig::new()
        .map_err(|()| Error::Encode("libwebp: не удалось создать параметры".into()))?;
    config.quality = quality as f32;
    // Метод 3 заметно быстрее стандартного 4 при почти том же размере.
    config.method = 3;
    config.thread_level = (threads > 1) as i32;
    let encoded = encoder
        .encode_advanced(&config)
        .map_err(|e| Error::Encode(format!("libwebp: {e:?}")))?;
    let file = webp_mux::with_metadata(
        &encoded,
        img.width,
        img.height,
        img.pixels.has_alpha(),
        img.icc.as_deref(),
        img.exif.as_deref(),
        img.xmp.as_deref(),
    )
    .map_err(Error::Encode)?;
    let mut out = create(dest)?;
    out.write_all(&file).at(dest)?;
    out.flush().at(dest)?;
    Ok(())
}
