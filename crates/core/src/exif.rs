//! Минимальная работа с EXIF (TIFF-структурой) без перекодирования блока:
//! сброс ориентации (FR-2.3), удаление GPS (FR-2.2), чтение даты съёмки (FR-4.2),
//! встроенная миниатюра для сравнения фото (FR-9.1).
//!
//! Блок правится на месте: длины и смещения не меняются, поэтому остальные теги
//! (в т.ч. MakerNote со внутренними смещениями) остаются корректными.

const TAG_ORIENTATION: u16 = 0x0112;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_DATE_TIME_ORIGINAL: u16 = 0x9003;
const TAG_THUMBNAIL_OFFSET: u16 = 0x0201;
const TAG_THUMBNAIL_LENGTH: u16 = 0x0202;
const TYPE_SHORT: u16 = 3;

/// Приводит EXIF из разных контейнеров к «голому» TIFF (`II*\0…` / `MM\0*…`).
///
/// - JPEG APP1 / WebP / PNG иногда содержат префикс `Exif\0\0`;
/// - в HEIF блок начинается с 4-байтного смещения до TIFF-заголовка.
pub fn normalize(raw: &[u8]) -> Option<Vec<u8>> {
    let mut data = raw;
    if data.len() >= 4 && !is_tiff(data) {
        let offset = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        if let Some(rest) = data.get(4 + offset..)
            && (is_tiff(rest) || rest.starts_with(b"Exif\0\0"))
        {
            data = rest;
        }
    }
    if let Some(rest) = data.strip_prefix(b"Exif\0\0") {
        data = rest;
    }
    is_tiff(data).then(|| data.to_vec())
}

fn is_tiff(d: &[u8]) -> bool {
    d.starts_with(b"II*\0") || d.starts_with(b"MM\0*")
}

struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    /// Смещение 12-байтной записи в блоке.
    pos: usize,
    tag: u16,
    typ: u16,
    count: u32,
    value: u32,
}

impl<'a> Tiff<'a> {
    fn new(d: &'a [u8]) -> Option<Self> {
        is_tiff(d).then(|| Tiff {
            d,
            le: d[0] == b'I',
        })
    }

    fn u16(&self, pos: usize) -> Option<u16> {
        let b = self.d.get(pos..pos + 2)?;
        Some(if self.le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32(&self, pos: usize) -> Option<u32> {
        let b = self.d.get(pos..pos + 4)?;
        Some(if self.le {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    }

    fn ifd0(&self) -> Option<usize> {
        self.u32(4).map(|v| v as usize)
    }

    fn entries(&self, ifd: usize) -> Option<Vec<Entry>> {
        let count = self.u16(ifd)? as usize;
        // Защита от зацикленных и битых IFD.
        if count > 1024 || ifd + 2 + count * 12 > self.d.len() {
            return None;
        }
        (0..count)
            .map(|i| {
                let pos = ifd + 2 + i * 12;
                Some(Entry {
                    pos,
                    tag: self.u16(pos)?,
                    typ: self.u16(pos + 2)?,
                    count: self.u32(pos + 4)?,
                    value: self.u32(pos + 8)?,
                })
            })
            .collect()
    }

    fn find(&self, ifd: usize, tag: u16) -> Option<Entry> {
        self.entries(ifd)?.into_iter().find(|e| e.tag == tag)
    }
}

fn type_size(typ: u16) -> usize {
    match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => 1,
    }
}

/// Значение тега Orientation (1–8), если есть.
pub fn orientation(tiff: &[u8]) -> Option<u16> {
    let t = Tiff::new(tiff)?;
    let e = t.find(t.ifd0()?, TAG_ORIENTATION)?;
    (e.typ == TYPE_SHORT).then(|| t.u16(e.pos + 8)).flatten()
}

/// Дата съёмки `DateTimeOriginal` в виде `YYYY:MM:DD HH:MM:SS`.
pub fn date_time_original(tiff: &[u8]) -> Option<String> {
    let t = Tiff::new(tiff)?;
    let exif_ifd = t.find(t.ifd0()?, TAG_EXIF_IFD)?.value as usize;
    let e = t.find(exif_ifd, TAG_DATE_TIME_ORIGINAL)?;
    let len = e.count as usize;
    let start = if len <= 4 {
        e.pos + 8
    } else {
        e.value as usize
    };
    let raw = tiff.get(start..start + len)?;
    let s = std::str::from_utf8(raw).ok()?.trim_end_matches('\0').trim();
    (s.len() >= 19).then(|| s[..19].to_string())
}

/// Встроенная JPEG-миниатюра из IFD1 (обычно 160×120) — у снимков с телефонов и камер.
/// Ориентация у неё та же, что у основного изображения.
pub fn thumbnail(tiff: &[u8]) -> Option<&[u8]> {
    let t = Tiff::new(tiff)?;
    let ifd0 = t.ifd0()?;
    let ifd1 = t.u32(ifd0 + 2 + t.entries(ifd0)?.len() * 12)? as usize;
    if ifd1 == 0 {
        return None;
    }
    let start = t.find(ifd1, TAG_THUMBNAIL_OFFSET)?.value as usize;
    let len = t.find(ifd1, TAG_THUMBNAIL_LENGTH)?.value as usize;
    let data = tiff.get(start..start.checked_add(len)?)?;
    data.starts_with(&[0xFF, 0xD8]).then_some(data)
}

/// Ставит Orientation = 1. Возвращает `false`, если блок не удалось разобрать.
pub fn reset_orientation(tiff: &mut [u8]) -> bool {
    let Some(t) = Tiff::new(tiff) else {
        return false;
    };
    let le = t.le;
    let Some(ifd0) = t.ifd0() else { return false };
    let Some(entries) = t.entries(ifd0) else {
        return false;
    };
    if let Some(e) = entries.iter().find(|e| e.tag == TAG_ORIENTATION) {
        if e.typ != TYPE_SHORT {
            return false;
        }
        let one = if le { [1, 0] } else { [0, 1] };
        tiff[e.pos + 8..e.pos + 10].copy_from_slice(&one);
    }
    true
}

/// Затирает GPS-IFD: данные тегов обнуляются, число записей становится 0.
/// Возвращает `false`, если блок не удалось разобрать.
pub fn strip_gps(tiff: &mut [u8]) -> bool {
    let Some(t) = Tiff::new(tiff) else {
        return false;
    };
    let Some(ifd0) = t.ifd0() else { return false };
    let Some(ifd0_entries) = t.entries(ifd0) else {
        return false;
    };
    let Some(gps_ptr) = ifd0_entries.iter().find(|e| e.tag == TAG_GPS_IFD) else {
        return true;
    };
    let gps_ifd = gps_ptr.value as usize;
    let Some(entries) = t.entries(gps_ifd) else {
        return false;
    };
    let mut ranges = Vec::with_capacity(entries.len());
    for e in &entries {
        let size = type_size(e.typ).saturating_mul(e.count as usize);
        if size > 4 {
            let start = e.value as usize;
            if start.checked_add(size).is_some_and(|end| end <= tiff.len()) {
                ranges.push(start..start + size);
            }
        }
    }
    for r in ranges {
        tiff[r].fill(0);
    }
    let table_end = gps_ifd + 2 + entries.len() * 12;
    tiff[gps_ifd..table_end].fill(0);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Строит little-endian TIFF: IFD0 {Orientation=6, ExifIFD, GPS-IFD},
    /// ExifIFD {DateTimeOriginal}, GPS-IFD {GPSLatitude (3×RATIONAL)}.
    fn sample(le: bool) -> Vec<u8> {
        let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let mut d = Vec::new();
        d.extend_from_slice(if le { b"II" } else { b"MM" });
        d.extend_from_slice(&w16(42));
        d.extend_from_slice(&w32(8));
        // IFD0 @8: 3 записи → 2 + 36 + 4 = 42 байта → до 50.
        let exif_ifd = 50u32;
        let gps_ifd = exif_ifd + 18; // 2 + 12 + 4
        let date_at = gps_ifd + 18;
        let lat_at = date_at + 20;
        d.extend_from_slice(&w16(3));
        for (tag, typ, count, value) in [
            (TAG_ORIENTATION, 3u16, 1u32, None),
            (TAG_EXIF_IFD, 4, 1, Some(exif_ifd)),
            (TAG_GPS_IFD, 4, 1, Some(gps_ifd)),
        ] {
            d.extend_from_slice(&w16(tag));
            d.extend_from_slice(&w16(typ));
            d.extend_from_slice(&w32(count));
            match value {
                Some(v) => d.extend_from_slice(&w32(v)),
                None => {
                    d.extend_from_slice(&w16(6));
                    d.extend_from_slice(&[0, 0]);
                }
            }
        }
        d.extend_from_slice(&w32(0));
        assert_eq!(d.len(), exif_ifd as usize);
        d.extend_from_slice(&w16(1));
        d.extend_from_slice(&w16(TAG_DATE_TIME_ORIGINAL));
        d.extend_from_slice(&w16(2));
        d.extend_from_slice(&w32(20));
        d.extend_from_slice(&w32(date_at));
        d.extend_from_slice(&w32(0));
        assert_eq!(d.len(), gps_ifd as usize);
        d.extend_from_slice(&w16(1));
        d.extend_from_slice(&w16(0x0002));
        d.extend_from_slice(&w16(5));
        d.extend_from_slice(&w32(3));
        d.extend_from_slice(&w32(lat_at));
        d.extend_from_slice(&w32(0));
        d.extend_from_slice(b"2024:05:17 10:20:30\0");
        for v in [55u32, 1, 45, 1, 3000, 100] {
            d.extend_from_slice(&w32(v));
        }
        d
    }

    #[test]
    fn reads_and_patches_both_byte_orders() {
        for le in [true, false] {
            let mut d = sample(le);
            let len = d.len();
            assert_eq!(orientation(&d), Some(6));
            assert_eq!(
                date_time_original(&d).as_deref(),
                Some("2024:05:17 10:20:30")
            );

            assert!(reset_orientation(&mut d));
            assert_eq!(orientation(&d), Some(1));

            assert!(strip_gps(&mut d));
            assert_eq!(d.len(), len);
            let t = Tiff::new(&d).unwrap();
            let gps = t.find(t.ifd0().unwrap(), TAG_GPS_IFD).unwrap().value as usize;
            assert_eq!(t.entries(gps).unwrap().len(), 0);
            // Координаты затёрты, дата осталась.
            assert!(d[d.len() - 24..].iter().all(|&b| b == 0));
            assert_eq!(
                date_time_original(&d).as_deref(),
                Some("2024:05:17 10:20:30")
            );
        }
    }

    #[test]
    fn finds_thumbnail_in_ifd1() {
        for le in [true, false] {
            let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
            let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
            let mut d = sample(le);
            assert_eq!(thumbnail(&d), None, "без IFD1");
            // Указатель на IFD1 — после записей IFD0 (8 + 2 + 3×12).
            let ifd1 = d.len() as u32;
            d[46..50].copy_from_slice(&w32(ifd1));
            let jpeg_at = ifd1 + 2 + 2 * 12 + 4;
            d.extend_from_slice(&w16(2));
            for (tag, value) in [(TAG_THUMBNAIL_OFFSET, jpeg_at), (TAG_THUMBNAIL_LENGTH, 4)] {
                d.extend_from_slice(&w16(tag));
                d.extend_from_slice(&w16(4));
                d.extend_from_slice(&w32(1));
                d.extend_from_slice(&w32(value));
            }
            d.extend_from_slice(&w32(0));
            d.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xD9]);
            assert_eq!(thumbnail(&d), Some(&[0xFF, 0xD8, 0xFF, 0xD9][..]));
            // Обрезанный блок — без паники.
            for cut in [50, 60, d.len() - 2] {
                assert_eq!(thumbnail(&d[..cut]), None);
            }
        }
    }

    #[test]
    fn normalizes_container_prefixes() {
        let tiff = sample(true);
        let mut jpeg = b"Exif\0\0".to_vec();
        jpeg.extend_from_slice(&tiff);
        assert_eq!(normalize(&jpeg).as_deref(), Some(tiff.as_slice()));

        let mut heif = 6u32.to_be_bytes().to_vec();
        heif.extend_from_slice(b"Exif\0\0");
        heif.extend_from_slice(&tiff);
        assert_eq!(normalize(&heif).as_deref(), Some(tiff.as_slice()));

        let mut heif0 = 0u32.to_be_bytes().to_vec();
        heif0.extend_from_slice(&tiff);
        assert_eq!(normalize(&heif0).as_deref(), Some(tiff.as_slice()));

        assert_eq!(normalize(b"garbage"), None);
    }

    #[test]
    fn survives_truncated_blocks() {
        let d = sample(true);
        for cut in [0, 4, 8, 20, 49, 60] {
            let mut part = d[..cut].to_vec();
            let _ = orientation(&part);
            let _ = date_time_original(&part);
            let _ = reset_orientation(&mut part);
            let _ = strip_gps(&mut part);
        }
    }
}
