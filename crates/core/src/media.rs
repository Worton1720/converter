//! Определение типа файла по содержимому (FR-1.3).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImageKind {
    Heif,
    Jpeg,
    Png,
    Webp,
    Bmp,
    Tiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaKind {
    Image(ImageKind),
    Video,
}

impl ImageKind {
    pub fn label(self) -> &'static str {
        match self {
            ImageKind::Heif => "HEIC",
            ImageKind::Jpeg => "JPEG",
            ImageKind::Png => "PNG",
            ImageKind::Webp => "WebP",
            ImageKind::Bmp => "BMP",
            ImageKind::Tiff => "TIFF",
        }
    }
}

impl MediaKind {
    pub fn is_image(self) -> bool {
        matches!(self, MediaKind::Image(_))
    }
}

/// Бренды ISO BMFF, означающие HEIF/HEIC/AVIF.
const HEIF_BRANDS: &[&[u8; 4]] = &[
    b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1", b"avif", b"avis",
];

/// Бренды ISO BMFF, означающие видео (MP4/MOV/3GP).
const VIDEO_BRANDS: &[&[u8; 4]] = &[
    b"qt  ", b"isom", b"iso2", b"iso4", b"iso5", b"iso6", b"mp41", b"mp42", b"M4V ", b"M4VH",
    b"avc1", b"dash", b"3gp4", b"3gp5", b"3gp6", b"3g2a", b"MSNV", b"XAVC", b"av01",
];

/// Расширения RAW: у DNG и многих RAW сигнатура TIFF, но в v1 они не поддерживаются.
const RAW_EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "pef", "srw",
];

const VIDEO_EXTENSIONS: &[&str] = &[
    "mov", "mp4", "m4v", "mkv", "webm", "avi", "3gp", "3g2", "mts", "m2ts", "ts", "hevc", "h265",
    "h264", "264", "265", "wmv", "flv", "mpg", "mpeg",
];

/// Определяет тип по первым байтам. `None` — не медиафайл или не поддерживается.
pub fn detect(path: &Path) -> Option<MediaKind> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if RAW_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let mut head = [0u8; 64];
    let n = File::open(path)
        .and_then(|mut f| read_up_to(&mut f, &mut head))
        .ok()?;
    detect_bytes(&head[..n]).or_else(|| {
        // Сырые потоки (annex B) и экзотические контейнеры отдаём ffprobe по расширению.
        VIDEO_EXTENSIONS
            .contains(&ext.as_str())
            .then_some(MediaKind::Video)
    })
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match f.read(&mut buf[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

pub fn detect_bytes(b: &[u8]) -> Option<MediaKind> {
    use ImageKind::*;
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(MediaKind::Image(Jpeg));
    }
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(MediaKind::Image(Png));
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" {
        return match &b[8..12] {
            b"WEBP" => Some(MediaKind::Image(Webp)),
            b"AVI " => Some(MediaKind::Video),
            _ => None,
        };
    }
    if b.starts_with(b"BM") && b.len() >= 14 {
        return Some(MediaKind::Image(Bmp));
    }
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        return Some(MediaKind::Image(Tiff));
    }
    if b.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(MediaKind::Video);
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        return detect_ftyp(b);
    }
    // QuickTime без ftyp: первый атом moov/mdat/wide/free.
    if b.len() >= 8 && matches!(&b[4..8], b"moov" | b"mdat" | b"wide" | b"free" | b"skip") {
        return Some(MediaKind::Video);
    }
    // MPEG-TS: синхробайт 0x47 через каждые 188 байт — в 64 байтах проверить нельзя,
    // поэтому .ts/.mts определяются по расширению в `detect`.
    None
}

fn detect_ftyp(b: &[u8]) -> Option<MediaKind> {
    let size = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    let end = size.clamp(12, b.len());
    let major: &[u8] = &b[8..12];
    // Совместимые бренды идут после major (4) и minor_version (4).
    let compatible = b.get(16..end).unwrap_or(&[]).as_chunks::<4>().0.iter();
    let is = |brands: &[&[u8; 4]], brand: &[u8]| brands.iter().any(|x| x.as_slice() == brand);

    if is(HEIF_BRANDS, major) {
        return Some(MediaKind::Image(ImageKind::Heif));
    }
    if is(VIDEO_BRANDS, major) {
        return Some(MediaKind::Video);
    }
    let compatible: Vec<&[u8]> = compatible.map(|c| c.as_slice()).collect();
    if compatible.iter().any(|c| is(HEIF_BRANDS, c)) {
        return Some(MediaKind::Image(ImageKind::Heif));
    }
    if compatible.iter().any(|c| is(VIDEO_BRANDS, c)) {
        return Some(MediaKind::Video);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp(major: &[u8; 4], compat: &[&[u8; 4]]) -> Vec<u8> {
        let size = 16 + 4 * compat.len();
        let mut v = (size as u32).to_be_bytes().to_vec();
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(major);
        v.extend_from_slice(&[0, 0, 0, 0]);
        for c in compat {
            v.extend_from_slice(*c);
        }
        v
    }

    #[test]
    fn detects_heic_and_video_brands() {
        assert_eq!(
            detect_bytes(&ftyp(b"heic", &[b"mif1", b"heic"])),
            Some(MediaKind::Image(ImageKind::Heif))
        );
        assert_eq!(
            detect_bytes(&ftyp(b"mif1", &[b"heic"])),
            Some(MediaKind::Image(ImageKind::Heif))
        );
        assert_eq!(
            detect_bytes(&ftyp(b"qt  ", &[b"qt  "])),
            Some(MediaKind::Video)
        );
        assert_eq!(
            detect_bytes(&ftyp(b"isom", &[b"mp41"])),
            Some(MediaKind::Video)
        );
        assert_eq!(
            detect_bytes(&ftyp(b"zzzz", &[b"mp42"])),
            Some(MediaKind::Video)
        );
    }

    #[test]
    fn detects_simple_signatures() {
        assert_eq!(
            detect_bytes(&[0xFF, 0xD8, 0xFF, 0xE1]),
            Some(MediaKind::Image(ImageKind::Jpeg))
        );
        assert_eq!(
            detect_bytes(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(MediaKind::Image(ImageKind::Webp))
        );
        assert_eq!(
            detect_bytes(b"RIFF\0\0\0\0AVI LIST"),
            Some(MediaKind::Video)
        );
        assert_eq!(detect_bytes(b"hello world"), None);
    }

    #[test]
    fn sample_files_are_detected_by_content() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data_media");
        if !dir.exists() {
            return;
        }
        assert_eq!(
            detect(&dir.join("фото отэц.HEIC")),
            Some(MediaKind::Image(ImageKind::Heif))
        );
        // Расширение .HEVC, но внутри контейнер MP4/MOV.
        assert_eq!(
            detect(&dir.join("Я-люблю-свою-работу-финал.HEVC")),
            Some(MediaKind::Video)
        );
        assert_eq!(detect(&dir.join("IMG_0822.MOV")), Some(MediaKind::Video));
    }
}
