//! Bounded raster metadata and local-link parsing. No decoder allocation,
//! remote fetch, XML parser, image transformation or host-path fallback.

use crew_core::rpc::docs::{
    DocImageReason, ImageLimitKind, RasterType, UnsupportedImageKind, MAX_IMAGE_BYTES,
};

use super::config::{Relative, MAX_PATH_BYTES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Raster {
    pub kind: RasterType,
    pub width: u32,
    pub height: u32,
}

fn invalid<T>() -> Result<T, DocImageReason> {
    Err(DocImageReason::Invalid)
}

/// Container structure and declared raster dimensions, without decompressing
/// pixels. The client still handles an ordinary image decode failure as a
/// placeholder; metadata validation is not a claim to repair corrupted pixels.
pub fn inspect(bytes: &[u8]) -> Result<Raster, DocImageReason> {
    if bytes.len() > MAX_IMAGE_BYTES as usize {
        return Err(DocImageReason::TooLarge(ImageLimitKind::ImageBytes));
    }
    let raster = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(bytes)?
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        gif(bytes)?
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        jpeg(bytes)?
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        webp(bytes)?
    } else {
        let head = std::str::from_utf8(&bytes[..bytes.len().min(1024)])
            .unwrap_or("")
            .trim_start_matches('\u{feff}')
            .trim_start();
        let kind = if head.starts_with("<svg") || head.starts_with("<?xml") {
            UnsupportedImageKind::Svg
        } else {
            UnsupportedImageKind::Format
        };
        return Err(DocImageReason::Unsupported(kind));
    };
    if raster.width == 0 || raster.height == 0 {
        return invalid();
    }
    if raster.width > 8192 || raster.height > 8192 {
        return Err(DocImageReason::TooLarge(ImageLimitKind::AxisPixels));
    }
    if u64::from(raster.width) * u64::from(raster.height) > 16_000_000 {
        return Err(DocImageReason::TooLarge(ImageLimitKind::TotalPixels));
    }
    Ok(raster)
}

fn be32(bytes: &[u8]) -> Result<u32, DocImageReason> {
    Ok(u32::from_be_bytes(
        bytes.try_into().map_err(|_| DocImageReason::Invalid)?,
    ))
}
fn le32(bytes: &[u8]) -> Result<u32, DocImageReason> {
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| DocImageReason::Invalid)?,
    ))
}
fn le16(bytes: &[u8]) -> Result<u32, DocImageReason> {
    Ok(u32::from(u16::from_le_bytes(
        bytes.try_into().map_err(|_| DocImageReason::Invalid)?,
    )))
}
fn be16(bytes: &[u8]) -> Result<u32, DocImageReason> {
    Ok(u32::from(u16::from_be_bytes(
        bytes.try_into().map_err(|_| DocImageReason::Invalid)?,
    )))
}
fn le24(bytes: &[u8]) -> Result<u32, DocImageReason> {
    let [a, b, c] = bytes else {
        return invalid();
    };
    Ok(u32::from(*a) | (u32::from(*b) << 8) | (u32::from(*c) << 16))
}

const CRC: [u32; 256] = {
    let mut table = [0; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 {
                0xedb88320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};
fn crc(bytes: &[u8]) -> u32 {
    let mut value = !0u32;
    for byte in bytes {
        value = CRC[((value ^ u32::from(*byte)) & 255) as usize] ^ (value >> 8);
    }
    !value
}

fn png(bytes: &[u8]) -> Result<Raster, DocImageReason> {
    let mut at = 8;
    let mut size = None;
    let mut pixels = false;
    let mut ended_pixels = false;
    let mut started_pixels = false;
    let mut palette = 0;
    let mut colour = 0;
    let mut depth = 0;
    while at < bytes.len() {
        let head = bytes.get(at..at + 8).ok_or(DocImageReason::Invalid)?;
        let len = be32(&head[..4])? as usize;
        let end = at
            .checked_add(12)
            .and_then(|v| v.checked_add(len))
            .ok_or(DocImageReason::Invalid)?;
        if end > bytes.len() {
            return invalid();
        }
        let kind = &head[4..8];
        let data = &bytes[at + 8..end - 4];
        if !kind.iter().all(u8::is_ascii_alphabetic)
            || crc(&bytes[at + 4..end - 4]) != be32(&bytes[end - 4..end])?
        {
            return invalid();
        }
        if at == 8 && kind != b"IHDR" {
            return invalid();
        }
        match kind {
            b"IHDR" => {
                if size.is_some() || data.len() != 13 {
                    return invalid();
                }
                let valid_depth = match data[9] {
                    0 => matches!(data[8], 1 | 2 | 4 | 8 | 16),
                    2 | 4 | 6 => matches!(data[8], 8 | 16),
                    3 => matches!(data[8], 1 | 2 | 4 | 8),
                    _ => false,
                };
                if !valid_depth || data[10] != 0 || data[11] != 0 || data[12] > 1 {
                    return invalid();
                }
                colour = data[9];
                depth = data[8];
                size = Some(Raster {
                    kind: RasterType::Png,
                    width: be32(&data[..4])?,
                    height: be32(&data[4..8])?,
                });
            }
            b"acTL" | b"fcTL" | b"fdAT" => {
                return Err(DocImageReason::Unsupported(UnsupportedImageKind::Animated))
            }
            b"PLTE" => {
                if started_pixels
                    || palette != 0
                    || data.is_empty()
                    || data.len() % 3 != 0
                    || data.len() > 768
                    || matches!(colour, 0 | 4)
                {
                    return invalid();
                }
                palette = data.len() / 3;
                if colour == 3 && palette > (1usize << depth) {
                    return invalid();
                }
            }
            b"IDAT" => {
                if ended_pixels || (colour == 3 && palette == 0) {
                    return invalid();
                }
                started_pixels = true;
                pixels |= !data.is_empty();
            }
            b"IEND" => {
                if !pixels || !data.is_empty() || end != bytes.len() {
                    return invalid();
                }
                return size.ok_or(DocImageReason::Invalid);
            }
            // Unknown critical chunks cannot establish a supported structure.
            _ if kind[0].is_ascii_uppercase() && kind != b"PLTE" => return invalid(),
            _ => {
                if started_pixels {
                    ended_pixels = true;
                }
            }
        }
        at = end;
    }
    invalid()
}

fn subblocks(bytes: &[u8], at: &mut usize) -> Result<(), DocImageReason> {
    loop {
        let n = usize::from(*bytes.get(*at).ok_or(DocImageReason::Invalid)?);
        *at += 1;
        if n == 0 {
            return Ok(());
        }
        *at = at
            .checked_add(n)
            .filter(|n| *n <= bytes.len())
            .ok_or(DocImageReason::Invalid)?;
    }
}

fn gif(bytes: &[u8]) -> Result<Raster, DocImageReason> {
    let head = bytes.get(..13).ok_or(DocImageReason::Invalid)?;
    let raster = Raster {
        kind: RasterType::Gif,
        width: le16(&head[6..8])?,
        height: le16(&head[8..10])?,
    };
    let mut at = 13
        + if head[10] & 0x80 != 0 {
            3usize << ((head[10] & 7) + 1)
        } else {
            0
        };
    let mut frames = 0;
    loop {
        let kind = *bytes.get(at).ok_or(DocImageReason::Invalid)?;
        at += 1;
        match kind {
            0x3b if at == bytes.len() && frames == 1 => return Ok(raster),
            0x2c => {
                frames += 1;
                if frames > 1 {
                    return Err(DocImageReason::Unsupported(UnsupportedImageKind::Animated));
                }
                let d = bytes.get(at..at + 9).ok_or(DocImageReason::Invalid)?;
                at += 9;
                let width = le16(&d[4..6])?;
                let height = le16(&d[6..8])?;
                if width == 0
                    || height == 0
                    || le16(&d[..2])? + width > raster.width
                    || le16(&d[2..4])? + height > raster.height
                {
                    return invalid();
                }
                if d[8] & 0x80 != 0 {
                    at += 3usize << ((d[8] & 7) + 1);
                }
                let lzw = *bytes.get(at).ok_or(DocImageReason::Invalid)?;
                at += 1;
                if !(2..=8).contains(&lzw) {
                    return invalid();
                }
                subblocks(bytes, &mut at)?;
            }
            0x21 => {
                let label = *bytes.get(at).ok_or(DocImageReason::Invalid)?;
                at += 1;
                if label == 0xff {
                    let n = usize::from(*bytes.get(at).ok_or(DocImageReason::Invalid)?);
                    let app = bytes
                        .get(at + 1..at + 1 + n)
                        .ok_or(DocImageReason::Invalid)?;
                    if app == b"NETSCAPE2.0" || app == b"ANIMEXTS1.0" {
                        return Err(DocImageReason::Unsupported(UnsupportedImageKind::Animated));
                    }
                }
                if label == 0xf9 && (bytes.get(at) != Some(&4) || bytes.get(at + 5) != Some(&0)) {
                    return invalid();
                }
                if !matches!(label, 0xf9 | 0xfe | 0xff) {
                    return invalid();
                }
                subblocks(bytes, &mut at)?;
            }
            _ => return invalid(),
        }
    }
}

fn jpeg(bytes: &[u8]) -> Result<Raster, DocImageReason> {
    let mut at = 2;
    let mut size = None;
    let mut scan = false;
    while at < bytes.len() {
        if bytes[at] != 0xff {
            if scan {
                at += 1;
                continue;
            }
            return invalid();
        }
        while bytes.get(at) == Some(&0xff) {
            at += 1;
        }
        let marker = *bytes.get(at).ok_or(DocImageReason::Invalid)?;
        at += 1;
        if scan && (marker == 0 || (0xd0..=0xd7).contains(&marker)) {
            continue;
        }
        if marker == 0xd9 {
            return if scan && at == bytes.len() {
                size.ok_or(DocImageReason::Invalid)
            } else {
                invalid()
            };
        }
        if matches!(marker, 0 | 0xd8 | 0x01 | 0xd0..=0xd7) {
            return invalid();
        }
        let len = be16(bytes.get(at..at + 2).ok_or(DocImageReason::Invalid)?)? as usize;
        if len < 2 {
            return invalid();
        }
        let payload = bytes.get(at + 2..at + len).ok_or(DocImageReason::Invalid)?;
        at += len;
        if matches!(marker, 0xc0..=0xc2) {
            if size.is_some()
                || payload.len() < 6
                || !matches!(payload[0], 8 | 12)
                || payload.len() != 6 + 3 * usize::from(payload[5])
                || !matches!(payload[5], 1 | 3 | 4)
            {
                return invalid();
            }
            size = Some(Raster {
                kind: RasterType::Jpeg,
                width: be16(&payload[3..5])?,
                height: be16(&payload[1..3])?,
            });
        } else if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            return invalid();
        }
        if marker == 0xda {
            if size.is_none() || payload.len() < 4 {
                return invalid();
            }
            scan = true;
        } else if scan {
            scan = false;
        }
    }
    invalid()
}

fn webp(bytes: &[u8]) -> Result<Raster, DocImageReason> {
    if bytes.len() < 12 || le32(&bytes[4..8])? as usize + 8 != bytes.len() {
        return invalid();
    }
    let mut at = 12;
    let mut dimensions = None;
    let mut canvas = None;
    while at < bytes.len() {
        let header = bytes.get(at..at + 8).ok_or(DocImageReason::Invalid)?;
        let length = le32(&header[4..8])? as usize;
        let end = at
            .checked_add(8)
            .and_then(|n| n.checked_add(length))
            .ok_or(DocImageReason::Invalid)?;
        let data = bytes.get(at + 8..end).ok_or(DocImageReason::Invalid)?;
        match &header[..4] {
            b"ANIM" | b"ANMF" => {
                return Err(DocImageReason::Unsupported(UnsupportedImageKind::Animated))
            }
            b"VP8X" => {
                if at != 12 || data.len() != 10 || data[0] & 0xc1 != 0 || data[1..4] != [0, 0, 0] {
                    return invalid();
                }
                if data[0] & 2 != 0 {
                    return Err(DocImageReason::Unsupported(UnsupportedImageKind::Animated));
                }
                canvas = Some((le24(&data[4..7])? + 1, le24(&data[7..10])? + 1));
            }
            b"VP8 " => {
                if dimensions.is_some()
                    || data.len() <= 10
                    || data[0] & 1 != 0
                    || data[3..6] != [0x9d, 0x01, 0x2a]
                {
                    return invalid();
                }
                dimensions = Some((le16(&data[6..8])? & 0x3fff, le16(&data[8..10])? & 0x3fff));
            }
            b"VP8L" => {
                if dimensions.is_some() || data.len() <= 5 || data[0] != 0x2f {
                    return invalid();
                }
                let bits = le32(&data[1..5])?;
                if bits >> 29 != 0 {
                    return invalid();
                }
                dimensions = Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1));
            }
            b"ALPH" | b"ICCP" | b"EXIF" | b"XMP " => {
                if canvas.is_none() {
                    return invalid();
                }
            }
            _ => return invalid(),
        }
        at = end + (length & 1);
        if at > bytes.len() || (length & 1 != 0 && bytes[end] != 0) {
            return invalid();
        }
    }
    let (width, height) = dimensions.ok_or(DocImageReason::Invalid)?;
    if canvas.is_some_and(|c| c != (width, height)) {
        return invalid();
    }
    Ok(Raster {
        kind: RasterType::Webp,
        width,
        height,
    })
}

/// Decode once, then normalize within the source. A parent component in a link
/// can name a sibling directory, but can never escape the registered root.
pub fn local_target(
    document: &Relative,
    destination: &str,
) -> Result<(Relative, Option<String>), &'static str> {
    if destination.len() > 2048
        || destination.starts_with('/')
        || destination.contains('\\')
        || destination.contains('?')
    {
        return Err("invalid local link");
    }
    let (path, fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(p, f)| (p, Some(f)));
    let path = decode(path)?;
    if path.starts_with('/') || path.contains(':') {
        return Err("local link is not a relative path");
    }
    let mut parts: Vec<&str> = document.as_str().split('/').collect();
    parts.pop();
    if path.is_empty() {
        return Ok((document.clone(), fragment.map(decode).transpose()?));
    }
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("link leaves documentation source");
                }
            }
            _ => parts.push(part),
        }
    }
    let normalized = parts.join("/");
    if normalized.len() > MAX_PATH_BYTES {
        return Err("link path exceeds limit");
    }
    Ok((
        Relative::new(&normalized)?,
        fragment.map(decode).transpose()?,
    ))
}

pub fn decode(value: &str) -> Result<String, &'static str> {
    let hex = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let a = bytes
                .get(at + 1)
                .and_then(|b| hex(*b))
                .ok_or("malformed link escape")?;
            let b = bytes
                .get(at + 2)
                .and_then(|b| hex(*b))
                .ok_or("malformed link escape")?;
            out.push((a << 4) | b);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    let decoded = String::from_utf8(out).map_err(|_| "link is not UTF-8")?;
    if decoded.contains('\\') || decoded.chars().any(char::is_control) {
        return Err("invalid link character");
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = include_bytes!("fixtures/pixel.png");
    const GIF: &[u8] = include_bytes!("fixtures/pixel.gif");
    const JPEG: &[u8] = include_bytes!("fixtures/pixel.jpg");
    const WEBP: &[u8] = include_bytes!("fixtures/pixel.webp");

    #[test]
    fn k07_real_static_raster_headers_have_bounded_dimensions() {
        for (data, kind) in [
            (PNG, RasterType::Png),
            (GIF, RasterType::Gif),
            (JPEG, RasterType::Jpeg),
            (WEBP, RasterType::Webp),
        ] {
            assert_eq!(
                inspect(data),
                Ok(Raster {
                    kind,
                    width: 2,
                    height: 2
                }),
                "{kind:?}"
            );
            for end in 0..data.len() {
                assert!(
                    inspect(&data[..end]).is_err(),
                    "accepted truncated {kind:?} at{end}"
                );
            }
            let mut tail = data.to_vec();
            tail.push(0);
            assert!(inspect(&tail).is_err(), "accepted trailing {kind:?} data");
        }
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut bytes = (data.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(&crc(&bytes[4..]).to_be_bytes());
        bytes
    }

    #[test]
    fn k07_animation_svg_and_metadata_overflow_are_placeholders() {
        assert_eq!(
            inspect(b"<svg onload='run()'/>"),
            Err(DocImageReason::Unsupported(UnsupportedImageKind::Svg))
        );
        let mut animation = PNG[..33].to_vec();
        animation.extend(chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
        animation.extend_from_slice(&PNG[33..]);
        assert_eq!(
            inspect(&animation),
            Err(DocImageReason::Unsupported(UnsupportedImageKind::Animated))
        );
        let mut huge = PNG.to_vec();
        huge[16..20].copy_from_slice(&9000u32.to_be_bytes());
        let checksum = crc(&huge[12..29]);
        huge[29..33].copy_from_slice(&checksum.to_be_bytes());
        assert_eq!(
            inspect(&huge),
            Err(DocImageReason::TooLarge(ImageLimitKind::AxisPixels))
        );
        huge[16..20].copy_from_slice(&5000u32.to_be_bytes());
        huge[20..24].copy_from_slice(&5000u32.to_be_bytes());
        let checksum = crc(&huge[12..29]);
        huge[29..33].copy_from_slice(&checksum.to_be_bytes());
        assert_eq!(
            inspect(&huge),
            Err(DocImageReason::TooLarge(ImageLimitKind::TotalPixels))
        );
        assert_eq!(
            inspect(&vec![0; MAX_IMAGE_BYTES as usize + 1]),
            Err(DocImageReason::TooLarge(ImageLimitKind::ImageBytes))
        );
        let mut corrupt = PNG.to_vec();
        corrupt[29] ^= 1;
        assert_eq!(inspect(&corrupt), Err(DocImageReason::Invalid));
        assert_eq!(crc(b"IEND"), 0xae426082);
        let mut anim_webp = b"RIFF".to_vec();
        anim_webp.extend_from_slice(&22u32.to_le_bytes());
        anim_webp.extend_from_slice(b"WEBPVP8X");
        anim_webp.extend_from_slice(&10u32.to_le_bytes());
        anim_webp.extend_from_slice(&[2, 0, 0, 0, 1, 0, 0, 1, 0, 0]);
        assert_eq!(
            inspect(&anim_webp),
            Err(DocImageReason::Unsupported(UnsupportedImageKind::Animated))
        );
        let mut gif_loop = GIF[..GIF.len() - 1].to_vec();
        gif_loop.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\0\0\0\x3b");
        assert_eq!(
            inspect(&gif_loop),
            Err(DocImageReason::Unsupported(UnsupportedImageKind::Animated))
        );
    }

    #[test]
    fn k04_k18_links_decode_once_and_never_escape_the_source() {
        let doc = Relative::new("notes/deep/doc.md").unwrap();
        let (path, fragment) = local_target(&doc, "../sibling.md#part%20one").unwrap();
        assert_eq!(path.as_str(), "notes/sibling.md");
        assert_eq!(fragment.as_deref(), Some("part one"));
        for value in [
            "../../../outside",
            "%2e%2e/%2e%2e/%2e%2e/outside",
            "/etc/passwd",
            "file:/tmp/x",
            "//server/path",
            "%2fetc/passwd",
            "..%5coutside",
            "a%00b",
            "bad%xx",
        ] {
            assert!(local_target(&doc, value).is_err(), "{value}");
        }
        assert_eq!(
            local_target(&doc, "%252e%252e/file.md").unwrap().0.as_str(),
            "notes/deep/%2e%2e/file.md"
        );
    }
}
