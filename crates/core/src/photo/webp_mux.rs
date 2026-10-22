//! Добавление ICC/EXIF/XMP в WebP: простой формат (VP8/VP8L) переупаковывается
//! в расширенный (VP8X) по спецификации RIFF-контейнера WebP.

const FLAG_ICC: u8 = 0x20;
const FLAG_ALPHA: u8 = 0x10;
const FLAG_EXIF: u8 = 0x08;
const FLAG_XMP: u8 = 0x04;

struct Chunk<'a> {
    fourcc: [u8; 4],
    data: &'a [u8],
}

fn parse(file: &[u8]) -> Result<Vec<Chunk<'_>>, String> {
    if file.len() < 12 || &file[0..4] != b"RIFF" || &file[8..12] != b"WEBP" {
        return Err("libwebp вернул не WebP".into());
    }
    let mut chunks = Vec::new();
    let mut pos = 12;
    while pos + 8 <= file.len() {
        let fourcc = [file[pos], file[pos + 1], file[pos + 2], file[pos + 3]];
        let size = u32::from_le_bytes([file[pos + 4], file[pos + 5], file[pos + 6], file[pos + 7]])
            as usize;
        let start = pos + 8;
        let data = file
            .get(start..start + size)
            .ok_or("обрезанный чанк WebP")?;
        chunks.push(Chunk { fourcc, data });
        pos = start + size + (size & 1);
    }
    Ok(chunks)
}

fn push_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
}

pub fn with_metadata(
    encoded: &[u8],
    width: u32,
    height: u32,
    alpha: bool,
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
    xmp: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    if icc.is_none() && exif.is_none() && xmp.is_none() {
        return Ok(encoded.to_vec());
    }
    let chunks = parse(encoded)?;
    let mut flags = 0u8;
    // Флаг альфы берём из исходного VP8X (lossy + ALPH) или из самих пикселей (VP8L).
    if let Some(vp8x) = chunks.iter().find(|c| &c.fourcc == b"VP8X") {
        flags |= vp8x.data.first().copied().unwrap_or(0) & FLAG_ALPHA;
    } else if alpha {
        flags |= FLAG_ALPHA;
    }
    if icc.is_some() {
        flags |= FLAG_ICC;
    }
    if exif.is_some() {
        flags |= FLAG_EXIF;
    }
    if xmp.is_some() {
        flags |= FLAG_XMP;
    }
    let mut vp8x = vec![flags, 0, 0, 0];
    vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);

    let mut body = b"WEBP".to_vec();
    push_chunk(&mut body, b"VP8X", &vp8x);
    if let Some(icc) = icc {
        push_chunk(&mut body, b"ICCP", icc);
    }
    for c in chunks
        .iter()
        .filter(|c| matches!(&c.fourcc, b"ALPH" | b"VP8 " | b"VP8L"))
    {
        push_chunk(&mut body, &c.fourcc, c.data);
    }
    if let Some(exif) = exif {
        push_chunk(&mut body, b"EXIF", exif);
    }
    if let Some(xmp) = xmp {
        push_chunk(&mut body, b"XMP ", xmp);
    }
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_simple_webp_into_vp8x_with_metadata() {
        let rgb = vec![200u8; 7 * 5 * 3];
        let encoded = webp::Encoder::from_rgb(&rgb, 7, 5).encode(80.0);
        let icc = vec![1u8; 13];
        let exif = b"II*\0\x08\0\0\0\0\0\0\0".to_vec();
        let file = with_metadata(&encoded, 7, 5, false, Some(&icc), Some(&exif), None).unwrap();

        let chunks = parse(&file).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.fourcc).collect();
        assert_eq!(names, [*b"VP8X", *b"ICCP", *b"VP8 ", *b"EXIF"]);
        assert_eq!(chunks[0].data[0], FLAG_ICC | FLAG_EXIF);
        assert_eq!(chunks[1].data, icc.as_slice());
        let riff = u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff + 8, file.len());

        let decoded = ::image::load_from_memory(&file).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (7, 5));
    }
}
