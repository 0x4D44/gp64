//! A minimal PNG writer: 8-bit RGB with stored (uncompressed) deflate blocks.
use gp64_types::util::crc32;

/// Encodes `0RGB` pixels, row-major, as a PNG file.
pub fn encode(pixels: &[u32], width: usize, height: usize) -> Vec<u8> {
    assert_eq!(pixels.len(), width * height, "pixel count must match size");
    let mut raw = Vec::with_capacity(height * (1 + width * 3));
    for row in pixels.chunks(width.max(1)).take(height) {
        raw.push(0); // filter: none
        for &pixel in row {
            raw.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]);
        }
    }
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB, no interlace
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &header);
    chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    chunk(&mut png, b"IEND", &[]);
    png
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

/// A zlib stream of stored deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 0xFFFF;
    let mut out = Vec::with_capacity(data.len() + data.len() / BLOCK * 5 + 11);
    out.extend_from_slice(&[0x78, 0x01]);
    let mut blocks = data.chunks(BLOCK).peekable();
    if blocks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(block) = blocks.next() {
        let last = u8::from(blocks.peek().is_none());
        let len = block.len() as u16;
        out.push(last);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits a PNG into its chunks, checking each CRC.
    fn chunks(png: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut rest = &png[8..];
        let mut out = Vec::new();
        while !rest.is_empty() {
            let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
            let body = &rest[4..8 + len];
            let crc = u32::from_be_bytes(rest[8 + len..12 + len].try_into().unwrap());
            assert_eq!(crc32(body), crc, "chunk CRC");
            out.push((
                String::from_utf8(body[..4].to_vec()).unwrap(),
                body[4..].to_vec(),
            ));
            rest = &rest[12 + len..];
        }
        out
    }

    /// Reassembles stored deflate blocks and checks the Adler-32 trailer.
    fn inflate_stored(zlib: &[u8]) -> Vec<u8> {
        assert_eq!(&zlib[..2], &[0x78, 0x01]);
        assert_eq!(u16::from_be_bytes([zlib[0], zlib[1]]) % 31, 0);
        let mut at = 2;
        let mut out = Vec::new();
        loop {
            let last = zlib[at] & 1;
            let len = u16::from_le_bytes([zlib[at + 1], zlib[at + 2]]);
            let nlen = u16::from_le_bytes([zlib[at + 3], zlib[at + 4]]);
            assert_eq!(len, !nlen);
            out.extend_from_slice(&zlib[at + 5..at + 5 + usize::from(len)]);
            at += 5 + usize::from(len);
            if last == 1 {
                break;
            }
        }
        assert_eq!(
            u32::from_be_bytes(zlib[at..at + 4].try_into().unwrap()),
            adler32(&out)
        );
        assert_eq!(at + 4, zlib.len());
        out
    }

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(&[]), 1);
    }

    #[test]
    fn png_round_trips_through_stored_blocks() {
        // 320x200 spans three stored blocks.
        let (width, height) = (320, 200);
        let pixels: Vec<u32> = (0..width * height)
            .map(|i| (i as u32) * 0x0001_0203)
            .collect();
        let png = encode(&pixels, width, height);
        let chunks = chunks(&png);
        let names: Vec<_> = chunks.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["IHDR", "IDAT", "IEND"]);
        assert_eq!(&chunks[0].1[..8], &[0, 0, 1, 64, 0, 0, 0, 200]);
        assert_eq!(&chunks[0].1[8..], &[8, 2, 0, 0, 0]);
        let raw = inflate_stored(&chunks[1].1);
        assert_eq!(raw.len(), height * (1 + width * 3));
        let row = 1 + width * 3;
        let (y, x) = (123, 45);
        let pixel = pixels[y * width + x];
        assert_eq!(raw[y * row], 0);
        assert_eq!(
            &raw[y * row + 1 + x * 3..][..3],
            &[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]
        );
    }
}
