//! Масштабирование (FR-2.1) и преобразование в sRGB (FR-2.4).

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use lcms2::{Flags, Intent, PixelFormat, Profile, Transform};

use super::{Decoded, Pixels};
use crate::error::{Error, Result};

fn u16_to_bytes(v: Vec<u16>) -> Vec<u8> {
    v.into_iter().flat_map(u16::to_ne_bytes).collect()
}

fn bytes_to_u16(v: Vec<u8>) -> Vec<u16> {
    v.as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_ne_bytes([b[0], b[1]]))
        .collect()
}

/// Уменьшает так, чтобы большая сторона стала `max_side`.
pub fn resize(mut img: Decoded, max_side: u32) -> Result<Decoded> {
    let longest = img.width.max(img.height);
    let scale = max_side as f64 / longest as f64;
    let nw = ((img.width as f64 * scale).round() as u32).max(1);
    let nh = ((img.height as f64 * scale).round() as u32).max(1);

    let (buf, pt) = match std::mem::replace(&mut img.pixels, Pixels::Rgb8(Vec::new())) {
        Pixels::Rgb8(v) => (v, PixelType::U8x3),
        Pixels::Rgba8(v) => (v, PixelType::U8x4),
        Pixels::Rgb16(v) => (u16_to_bytes(v), PixelType::U16x3),
        Pixels::Rgba16(v) => (u16_to_bytes(v), PixelType::U16x4),
    };
    let src = Image::from_vec_u8(img.width, img.height, buf, pt)
        .map_err(|e| Error::Encode(e.to_string()))?;
    let mut dst = Image::new(nw, nh, pt);
    Resizer::new()
        .resize(
            &src,
            &mut dst,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3)),
        )
        .map_err(|e| Error::Encode(e.to_string()))?;
    let out = dst.into_vec();
    img.pixels = match pt {
        PixelType::U8x3 => Pixels::Rgb8(out),
        PixelType::U8x4 => Pixels::Rgba8(out),
        PixelType::U16x3 => Pixels::Rgb16(bytes_to_u16(out)),
        _ => Pixels::Rgba16(bytes_to_u16(out)),
    };
    img.width = nw;
    img.height = nh;
    Ok(img)
}

/// Переводит пиксели из встроенного профиля в sRGB и убирает профиль.
/// Без профиля изображение и так считается sRGB.
pub fn to_srgb(mut img: Decoded) -> Result<Decoded> {
    let Some(icc) = img.icc.take() else {
        return Ok(img);
    };
    let src = Profile::new_icc(&icc).map_err(|e| Error::Encode(format!("ICC: {e}")))?;
    let dst = Profile::new_srgb();
    let intent = Intent::RelativeColorimetric;
    let err = |e: lcms2::Error| Error::Encode(format!("lcms2: {e}"));
    let flags = Flags::BLACKPOINT_COMPENSATION | Flags::COPY_ALPHA;
    match &mut img.pixels {
        Pixels::Rgb8(v) => {
            let t: Transform<[u8; 3], [u8; 3]> = Transform::new_flags(
                &src,
                PixelFormat::RGB_8,
                &dst,
                PixelFormat::RGB_8,
                intent,
                flags,
            )
            .map_err(err)?;
            t.transform_in_place(v.as_chunks_mut::<3>().0);
        }
        Pixels::Rgba8(v) => {
            let t: Transform<[u8; 4], [u8; 4]> = Transform::new_flags(
                &src,
                PixelFormat::RGBA_8,
                &dst,
                PixelFormat::RGBA_8,
                intent,
                flags,
            )
            .map_err(err)?;
            t.transform_in_place(v.as_chunks_mut::<4>().0);
        }
        Pixels::Rgb16(v) => {
            let t: Transform<[u16; 3], [u16; 3]> = Transform::new_flags(
                &src,
                PixelFormat::RGB_16,
                &dst,
                PixelFormat::RGB_16,
                intent,
                flags,
            )
            .map_err(err)?;
            t.transform_in_place(v.as_chunks_mut::<3>().0);
        }
        Pixels::Rgba16(v) => {
            let t: Transform<[u16; 4], [u16; 4]> = Transform::new_flags(
                &src,
                PixelFormat::RGBA_16,
                &dst,
                PixelFormat::RGBA_16,
                intent,
                flags,
            )
            .map_err(err)?;
            t.transform_in_place(v.as_chunks_mut::<4>().0);
        }
    }
    Ok(img)
}
