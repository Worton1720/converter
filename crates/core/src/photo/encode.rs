use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use jpeg_encoder::{ColorType, Encoder as JpegEncoder, SamplingFactor};

use super::webp_mux;
use super::{Decoded, Pixels};
use crate::error::{Error, IoContext, Result};
use crate::options::PngCompression;

/// Предел полезной нагрузки сегмента APP1 в JPEG.
const JPEG_APP1_MAX: usize = 65_533;
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

pub fn jpeg(img: &Decoded, dest: &Path, quality: u8) -> Result<()> {
    let Pixels::Rgb8(data) = &img.pixels else {
        return Err(Error::Encode("JPEG ожидает RGB 8 бит".into()));
    };
    let mut out = create(dest)?;
    let mut encoder = JpegEncoder::new(&mut out, quality.clamp(1, 100));
    encoder.set_sampling_factor(SamplingFactor::F_2_2);
    if let Some(icc) = &img.icc {
        encoder.add_icc_profile(icc).map_err(encode_err)?;
    }
    if let Some(exif) = img.exif.as_ref().filter(|e| e.len() + 6 <= JPEG_APP1_MAX) {
        encoder.add_exif_metadata(exif).map_err(encode_err)?;
    }
    if let Some(xmp) = img
        .xmp
        .as_ref()
        .filter(|x| x.len() + XMP_JPEG_HEADER.len() <= JPEG_APP1_MAX)
    {
        let mut segment = XMP_JPEG_HEADER.to_vec();
        segment.extend_from_slice(xmp);
        encoder.add_app_segment(1, segment).map_err(encode_err)?;
    }
    encoder
        .encode(data, img.width as u16, img.height as u16, ColorType::Rgb)
        .map_err(encode_err)?;
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
