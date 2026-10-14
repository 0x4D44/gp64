//! Utility functions for string parsing and escape handling.
//!
//! These are pure functions extracted from main.rs for testability.

/// Parse a string with escape sequences into raw bytes.
///
/// Recognised escapes: `\n`, `\r`, `\t`, `\\`, `\xNN` (2 hex digits). Any other
/// `\X` sequence is preserved verbatim (backslash kept), so a hand-typed
/// Windows path such as `C:\WINDOWS` survives intact instead of silently losing
/// its separators.
///
/// # Examples
/// ```
/// use gp64_types::util::parse_escaped_bytes;
/// assert_eq!(parse_escaped_bytes("hello\\nworld"), b"hello\nworld");
/// assert_eq!(parse_escaped_bytes("\\x1B[0m"), b"\x1B[0m");
/// assert_eq!(parse_escaped_bytes("C:\\WINDOWS"), b"C:\\WINDOWS");
/// ```
pub fn parse_escaped_bytes(s: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }

        i += 1;
        if i >= bytes.len() {
            out.push(b'\\');
            break;
        }

        match bytes[i] {
            b'\\' => out.push(b'\\'),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'x' => {
                if i + 2 < bytes.len() {
                    let hi = bytes[i + 1];
                    let lo = bytes[i + 2];
                    let hex = |c: u8| -> Option<u8> {
                        match c {
                            b'0'..=b'9' => Some(c - b'0'),
                            b'a'..=b'f' => Some(c - b'a' + 10),
                            b'A'..=b'F' => Some(c - b'A' + 10),
                            _ => None,
                        }
                    };
                    if let (Some(hi), Some(lo)) = (hex(hi), hex(lo)) {
                        out.push((hi << 4) | lo);
                        i += 2;
                    } else {
                        out.extend_from_slice(b"\\x");
                    }
                } else {
                    out.extend_from_slice(b"\\x");
                }
            }
            // Unknown escape: preserve the backslash *and* the following
            // character verbatim. Dropping the backslash silently corrupts
            // hand-typed Windows paths (`type "C:\WINDOWS"` -> `C:WINDOWS`),
            // which reads downstream as a "dropped keystroke". Recognised
            // escapes (`\\`, `\n`, `\r`, `\t`, `\xNN`) are handled above.
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
        i += 1;
    }

    out
}

/// Split a "PREFIX:PAYLOAD" string and parse the prefix as a numeric type.
///
/// `format_hint` appears in "Expected ..." messages (e.g. "FRAME:STRING").
/// `label_lc` is the lowercase variant for "Invalid ..." messages (e.g. "frame").
fn split_at_prefix<'a, T: core::str::FromStr<Err = E>, E: core::fmt::Display>(
    s: &'a str,
    format_hint: &str,
    label_lc: &str,
) -> Result<(T, &'a str), String> {
    let (prefix_s, payload) = s
        .split_once(':')
        .ok_or_else(|| format!("Expected {format_hint}"))?;
    let value = prefix_s
        .trim()
        .parse::<T>()
        .map_err(|e| format!("Invalid {label_lc} '{prefix_s}': {e}"))?;
    Ok((value, payload))
}

/// Auto mouse event (absolute coordinates and optional button state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoMouseEvent {
    pub x: i16,
    pub y: i16,
    pub buttons: Option<u8>,
}

/// Parse a "PREFIX:X,Y[,BUTTONS]" spec, returning the parsed prefix and mouse event.
fn parse_mouse_at_generic<T: core::str::FromStr<Err = E>, E: core::fmt::Display>(
    s: &str,
    format_hint: &str,
    label_lc: &str,
) -> Result<(T, AutoMouseEvent), String> {
    let (n, payload) = split_at_prefix::<T, E>(s, format_hint, label_lc)?;

    let parts: Vec<_> = payload.split(',').map(str::trim).collect();

    if parts.len() > 3 {
        return Err(format!("Too many fields for {format_hint}"));
    }

    let x_part = if let Some(value) = parts.first().filter(|value| !value.is_empty()) {
        *value
    } else {
        return Err("Expected X coordinate".to_string());
    };
    let y_part = if let Some(value) = parts.get(1).filter(|value| !value.is_empty()) {
        *value
    } else {
        return Err("Expected Y coordinate".to_string());
    };
    let x = parse_coord_i16(x_part)?;
    let y = parse_coord_i16(y_part)?;
    let buttons = match parts.get(2).copied() {
        Some("") => return Err("Expected buttons".to_string()),
        Some(btn_s) => Some(parse_buttons_u8(btn_s)? & 0x07),
        None => None,
    };

    Ok((n, AutoMouseEvent { x, y, buttons }))
}

fn parse_coord_i16(value: &str) -> Result<i16, String> {
    let value = value.trim();
    let lower_hex = value.strip_prefix("0x");
    let upper_hex = value.strip_prefix("0X");
    let parsed = if let Some(hex) = lower_hex.or(upper_hex) {
        i32::from_str_radix(hex, 16).map_err(|e| format!("Invalid value '{value}': {e}"))?
    } else {
        value
            .parse::<i32>()
            .map_err(|e| format!("Invalid value '{value}': {e}"))?
    };

    if parsed < i16::MIN as i32 || parsed > i16::MAX as i32 {
        return Err(format!("Value out of range for i16: {parsed}"));
    }

    Ok(parsed as i16)
}

/// Parse an auto-mouse-at string in the format "FRAME:X,Y[,BUTTONS]".
///
/// BUTTONS is a bitmask: 1=left, 2=right, 4=middle (optional).
///
/// # Examples
/// ```
/// use gp64_types::util::{parse_auto_mouse_at, AutoMouseEvent};
/// let (frame, evt) = parse_auto_mouse_at("120:160,100,1").unwrap();
/// assert_eq!(frame, 120);
/// assert_eq!(evt, AutoMouseEvent { x: 160, y: 100, buttons: Some(1) });
/// ```
pub fn parse_auto_mouse_at(s: &str) -> Result<(u32, AutoMouseEvent), String> {
    parse_mouse_at_generic(s, "FRAME:X,Y[,BUTTONS]", "frame")
}

/// Parse a tick-based auto-mouse-at string in the format "TICK:X,Y[,BUTTONS]".
///
/// Same as `parse_auto_mouse_at` but uses u64 tick numbers.
pub fn parse_auto_mouse_at_tick(s: &str) -> Result<(u64, AutoMouseEvent), String> {
    parse_mouse_at_generic(s, "TICK:X,Y[,BUTTONS]", "tick")
}

fn parse_buttons_u8(value: &str) -> Result<u8, String> {
    let value = value.trim();
    let lower_hex = value.strip_prefix("0x");
    let upper_hex = value.strip_prefix("0X");
    if let Some(hex) = lower_hex.or(upper_hex) {
        return u8::from_str_radix(hex, 16).map_err(|e| format!("Invalid buttons '{value}': {e}"));
    }

    value
        .parse::<u8>()
        .map_err(|e| format!("Invalid buttons '{value}': {e}"))
}

/// Check if an ASCII character requires the Shift key on a US keyboard.
///
/// Returns `Some(base_char)` if shift is needed, where `base_char` is the
/// unshifted character that produces this character when shifted.
/// Returns `None` if no shift is needed.
///
/// # Examples
/// ```
/// use gp64_types::util::ascii_needs_shift;
/// assert_eq!(ascii_needs_shift(b'A'), Some(b'a'));  // Uppercase needs shift
/// assert_eq!(ascii_needs_shift(b'!'), Some(b'1'));  // ! is Shift+1
/// assert_eq!(ascii_needs_shift(b'a'), None);        // lowercase, no shift
/// ```
pub fn ascii_needs_shift(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b + 32), // uppercase -> lowercase
        b'!' => Some(b'1'),
        b'@' => Some(b'2'),
        b'#' => Some(b'3'),
        b'$' => Some(b'4'),
        b'%' => Some(b'5'),
        b'^' => Some(b'6'),
        b'&' => Some(b'7'),
        b'*' => Some(b'8'),
        b'(' => Some(b'9'),
        b')' => Some(b'0'),
        b'_' => Some(b'-'),
        b'+' => Some(b'='),
        b'{' => Some(b'['),
        b'}' => Some(b']'),
        b'|' => Some(b'\\'),
        b':' => Some(b';'),
        b'"' => Some(b'\''),
        b'<' => Some(b','),
        b'>' => Some(b'.'),
        b'?' => Some(b'/'),
        b'~' => Some(b'`'),
        _ => None,
    }
}

const fn crc32_ieee_table() -> [u32; 256] {
    const REFLECTED_POLYNOMIAL: u32 = 0xEDB8_8320;
    let mut table = [0; 256];
    let mut index = 0;
    while index < table.len() {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ REFLECTED_POLYNOMIAL
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

const CRC32_IEEE_TABLE: [u32; 256] = crc32_ieee_table();

/// Compute the standard IEEE CRC-32 of a framebuffer pixel array.
/// Hashes only the RGB bytes (3 per pixel, R-G-B order), ignoring the
/// high byte of each u32. Matches the algorithm used by verify_frame_hash().
pub fn framebuffer_crc32(fb: &[u32]) -> u32 {
    let mut crc = u32::MAX;
    for &pixel in fb {
        crc = crc32_update(
            crc,
            &[
                ((pixel >> 16) & 0xFF) as u8,
                ((pixel >> 8) & 0xFF) as u8,
                (pixel & 0xFF) as u8,
            ],
        );
    }
    !crc
}

/// The standard IEEE CRC-32 of `data`, as used by zlib and PNG.
///
/// ```
/// assert_eq!(gp64_types::util::crc32(b"123456789"), 0xCBF4_3926);
/// ```
pub fn crc32(data: &[u8]) -> u32 {
    !crc32_update(u32::MAX, data)
}

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        let index = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_IEEE_TABLE[index];
    }
    crc
}

// THROWAWAY-SPIKE: Gremlin determinism Step-0.
// This helper is used by the `--gremlin-spike-trail-out` CLI shim to record
// per-PIT-tick framebuffer fingerprints. Remove together with the rest of
// the spike shim once the determinism data point lands.

/// Compute BLAKE3 of a framebuffer pixel array. Hashes the same RGB bytes
/// (R, G, B in that order) as [`framebuffer_crc32`] so a divergence between
/// hashes can only mean the pixel bytes themselves differ.
///
/// Returns the 32-byte BLAKE3 output. Use [`framebuffer_blake3_hex`] when a
/// stable text representation is needed (e.g. trail-file output).
pub fn framebuffer_blake3(fb: &[u32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for &pixel in fb {
        let r = ((pixel >> 16) & 0xFF) as u8;
        let g = ((pixel >> 8) & 0xFF) as u8;
        let b = (pixel & 0xFF) as u8;
        hasher.update(&[r, g, b]);
    }
    hasher.finalize().into()
}

/// Lower-case hex encoding of [`framebuffer_blake3`]. 64 ASCII chars.
pub fn framebuffer_blake3_hex(fb: &[u32]) -> String {
    let bytes = framebuffer_blake3(fb);
    let mut s = String::with_capacity(64);
    for b in bytes {
        // Write each byte as two lowercase hex digits. Avoid `write!`
        // formatter overhead — the spike shim emits this on every PIT tick.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0F) as usize] as char);
    }
    s
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // ==================== framebuffer_crc32 tests ====================

    fn reference_framebuffer_crc32(fb: &[u32]) -> u32 {
        let mut crc = u32::MAX;
        for &pixel in fb {
            for byte in [
                ((pixel >> 16) & 0xFF) as u8,
                ((pixel >> 8) & 0xFF) as u8,
                (pixel & 0xFF) as u8,
            ] {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = if crc & 1 == 0 {
                        crc >> 1
                    } else {
                        (crc >> 1) ^ 0xEDB8_8320
                    };
                }
            }
        }
        !crc
    }

    #[test]
    fn test_framebuffer_crc32_matches_published_123456789_vector() {
        // Pack the published IEEE CRC32 vector bytes into RGB pixels.
        let pixels = [0x0031_3233, 0x0034_3536, 0x0037_3839];
        assert_eq!(framebuffer_crc32(&pixels), 0xCBF4_3926);
    }

    #[test]
    fn test_framebuffer_crc32_matches_independent_reference_for_all_byte_values() {
        let mut bytes = (0u16..=255).map(|value| value as u8).collect::<Vec<_>>();
        // Two extra bytes make the RGB stream exactly divisible into pixels.
        bytes.extend([0, 1]);
        let (rgb_pixels, remainder) = bytes.as_chunks::<3>();
        assert!(remainder.is_empty());
        let pixels = rgb_pixels
            .iter()
            .enumerate()
            .map(|(index, rgb)| {
                (((index as u32).wrapping_mul(37) & 0xFF) << 24)
                    | (u32::from(rgb[0]) << 16)
                    | (u32::from(rgb[1]) << 8)
                    | u32::from(rgb[2])
            })
            .collect::<Vec<_>>();

        assert_eq!(bytes.len(), 258);
        assert_eq!(pixels.len(), 86);
        assert_eq!(
            framebuffer_crc32(&pixels),
            reference_framebuffer_crc32(&pixels)
        );
    }

    #[test]
    fn test_framebuffer_crc32_matches_independent_reference_at_length_boundaries() {
        for pixel_count in [
            0, 1, 2, 3, 7, 8, 9, 10, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129,
        ] {
            let pixels = (0..pixel_count)
                .map(|index| {
                    let index = index as u32;
                    (((index.wrapping_mul(37).wrapping_add(0xA5)) & 0xFF) << 24)
                        | (((index.wrapping_mul(73).wrapping_add(0x80)) & 0xFF) << 16)
                        | (((index.wrapping_mul(151).wrapping_add(0x7F)) & 0xFF) << 8)
                        | ((index.wrapping_mul(199).wrapping_add(0xFF)) & 0xFF)
                })
                .collect::<Vec<_>>();

            assert_eq!(
                framebuffer_crc32(&pixels),
                reference_framebuffer_crc32(&pixels),
                "pixel count {pixel_count}"
            );
        }
    }

    #[test]
    fn test_framebuffer_crc32_empty() {
        assert_eq!(framebuffer_crc32(&[]), 0x00000000);
    }

    // ==================== framebuffer_blake3 tests (Gremlin spike) ====================
    // THROWAWAY-SPIKE: delete with the rest of the spike shim once Step-0 closes.

    /// Published BLAKE3 hash of the empty input. Treat as a stability anchor.
    /// See https://github.com/BLAKE3-team/BLAKE3/blob/master/test_vectors/test_vectors.json
    const BLAKE3_EMPTY_HEX: &str =
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    #[test]
    fn test_framebuffer_blake3_empty_matches_published_vector() {
        assert_eq!(framebuffer_blake3_hex(&[]), BLAKE3_EMPTY_HEX);
    }

    #[test]
    fn test_framebuffer_blake3_single_black_pixel_matches_reference() {
        // One pixel of RGB(0,0,0) — feeds three zero bytes to BLAKE3.
        let got = framebuffer_blake3(&[0x00000000]);
        let expected: [u8; 32] = blake3::hash(&[0u8, 0u8, 0u8]).into();
        assert_eq!(got, expected);
    }

    #[test]
    fn test_framebuffer_blake3_ignores_high_byte() {
        // The high byte (alpha) is not hashed.
        let a = framebuffer_blake3(&[0x00FF0000]);
        let b = framebuffer_blake3(&[0xFFFF0000]);
        assert_eq!(a, b);
    }

    #[test]
    fn test_framebuffer_blake3_rgb_order_matches_crc32() {
        // Red pixel: R=0xFF, G=0x00, B=0x00 — same byte ordering as the
        // long-standing CRC32 helper so divergence is in pixels only.
        let got = framebuffer_blake3(&[0x00FF0000]);
        let expected: [u8; 32] = blake3::hash(&[0xFFu8, 0x00u8, 0x00u8]).into();
        assert_eq!(got, expected);
    }

    #[test]
    fn test_framebuffer_blake3_multiple_pixels_concatenated() {
        let pixels = [0x00FF0000, 0x0000FF00, 0x000000FF];
        let got = framebuffer_blake3(&pixels);
        let expected: [u8; 32] = blake3::hash(&[
            0xFF, 0x00, 0x00, // Red
            0x00, 0xFF, 0x00, // Green
            0x00, 0x00, 0xFF, // Blue
        ])
        .into();
        assert_eq!(got, expected);
    }

    #[test]
    fn test_framebuffer_blake3_deterministic() {
        let pixels = [0x00123456, 0x0078ABCD, 0x00EFBEEF];
        let a = framebuffer_blake3(&pixels);
        let b = framebuffer_blake3(&pixels);
        assert_eq!(a, b);
    }

    #[test]
    fn test_framebuffer_blake3_hex_is_64_lowercase_chars() {
        let h = framebuffer_blake3_hex(&[0x00123456]);
        assert_eq!(h.len(), 64);
        assert!(h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    }

    #[test]
    fn test_framebuffer_blake3_different_inputs_differ() {
        let a = framebuffer_blake3(&[0x00FF0000]);
        let b = framebuffer_blake3(&[0x0000FF00]);
        assert_ne!(a, b);
    }

    #[test]
    fn test_framebuffer_crc32_single_black_pixel() {
        // Black pixel: 0x00000000 -> RGB(0,0,0)
        assert_eq!(framebuffer_crc32(&[0x00000000]), 0xFF41_D912);
    }

    #[test]
    fn test_framebuffer_crc32_single_white_pixel() {
        // White pixel: 0x00FFFFFF -> RGB(255,255,255)
        assert_eq!(framebuffer_crc32(&[0x00FF_FFFF]), 0xFFFF_FF00);
    }

    #[test]
    fn test_framebuffer_crc32_ignores_high_byte() {
        // The high byte (alpha) should be ignored
        let crc_a = framebuffer_crc32(&[0x00FF0000]);
        let crc_b = framebuffer_crc32(&[0xFFFF0000]);
        assert_eq!(crc_a, crc_b);
    }

    #[test]
    fn test_framebuffer_crc32_rgb_decomposition() {
        // Red pixel: 0x00FF0000 -> R=255, G=0, B=0
        assert_eq!(framebuffer_crc32(&[0x00FF_0000]), 0x41D9_EDFF);
    }

    #[test]
    fn test_framebuffer_crc32_multiple_pixels() {
        let pixels = [0x00FF0000, 0x0000FF00, 0x000000FF];
        assert_eq!(framebuffer_crc32(&pixels), 0x4820_30E4);
    }

    #[test]
    fn test_framebuffer_crc32_deterministic() {
        let pixels = [0x00123456, 0x0078ABCD, 0x00EFBEEF];
        let crc1 = framebuffer_crc32(&pixels);
        let crc2 = framebuffer_crc32(&pixels);
        assert_eq!(crc1, crc2);
        assert_eq!(crc1, 0xA951_03E1);
    }

    #[test]
    fn test_framebuffer_crc32_different_inputs_differ() {
        let a = framebuffer_crc32(&[0x00FF0000]);
        let b = framebuffer_crc32(&[0x0000FF00]);
        assert_ne!(a, b);
    }

    // ==================== parse_escaped_bytes tests ====================

    #[test]
    fn test_parse_escaped_bytes_plain() {
        assert_eq!(parse_escaped_bytes("hello"), b"hello");
    }

    #[test]
    fn test_parse_escaped_bytes_empty() {
        assert_eq!(parse_escaped_bytes(""), b"");
    }

    #[test]
    fn test_parse_escaped_bytes_newline() {
        assert_eq!(parse_escaped_bytes("line1\\nline2"), b"line1\nline2");
    }

    #[test]
    fn test_parse_escaped_bytes_carriage_return() {
        assert_eq!(parse_escaped_bytes("line1\\rline2"), b"line1\rline2");
    }

    #[test]
    fn test_parse_escaped_bytes_tab() {
        assert_eq!(parse_escaped_bytes("col1\\tcol2"), b"col1\tcol2");
    }

    #[test]
    fn test_parse_escaped_bytes_backslash() {
        assert_eq!(parse_escaped_bytes("path\\\\to\\\\file"), b"path\\to\\file");
    }

    #[test]
    fn test_parse_escaped_bytes_hex_lowercase() {
        assert_eq!(parse_escaped_bytes("\\x1b[0m"), b"\x1b[0m");
    }

    #[test]
    fn test_parse_escaped_bytes_hex_uppercase() {
        assert_eq!(parse_escaped_bytes("\\x1B[0m"), b"\x1B[0m");
    }

    #[test]
    fn test_parse_escaped_bytes_hex_mixed() {
        assert_eq!(parse_escaped_bytes("\\xAb\\xCd"), b"\xAB\xCD");
    }

    #[test]
    fn test_parse_escaped_bytes_multiple_escapes() {
        assert_eq!(parse_escaped_bytes("\\r\\n\\t\\x00"), b"\r\n\t\x00");
    }

    #[test]
    fn test_parse_escaped_bytes_trailing_backslash() {
        assert_eq!(parse_escaped_bytes("test\\"), b"test\\");
    }

    #[test]
    fn test_parse_escaped_bytes_incomplete_hex_one_digit() {
        // \x with only one hex digit
        assert_eq!(parse_escaped_bytes("\\x1"), b"\\x1");
    }

    #[test]
    fn test_parse_escaped_bytes_incomplete_hex_no_digits() {
        // \x at end of string
        assert_eq!(parse_escaped_bytes("test\\x"), b"test\\x");
    }

    #[test]
    fn test_parse_escaped_bytes_invalid_hex() {
        // \xGG is not valid hex
        assert_eq!(parse_escaped_bytes("\\xGG"), b"\\xGG");
    }

    #[test]
    fn test_parse_escaped_bytes_unknown_escape() {
        // Unknown escape sequence: the backslash is preserved verbatim along
        // with the following character (Python-style). Dropping the backslash
        // silently corrupts hand-typed Windows paths (`type "C:\WINDOWS"`),
        // which previously parsed to `C:WINDOWS` and read as a "dropped key".
        assert_eq!(parse_escaped_bytes("\\q"), b"\\q");
    }

    #[test]
    fn test_parse_escaped_bytes_windows_path_single_backslash() {
        // A naively hand-typed Windows path with single backslashes must round
        // -trip unchanged. `\W` and `\S` are unknown escapes, so the backslash
        // is preserved (it is not a `\n`/`\r`/`\t`/`\\`/`\xNN` sequence).
        assert_eq!(
            parse_escaped_bytes("C:\\WINDOWS\\SYSTEM"),
            b"C:\\WINDOWS\\SYSTEM"
        );
    }

    #[test]
    fn test_parse_escaped_bytes_known_escapes_still_special() {
        // The robustness change must not weaken the recognised escapes: a
        // literal `\\` still collapses to one backslash, and `\n`/`\t` remain
        // control characters even when adjacent to path-like text.
        assert_eq!(parse_escaped_bytes("A:\\\\B"), b"A:\\B");
        assert_eq!(parse_escaped_bytes("x\\ny\\tz"), b"x\ny\tz");
    }

    #[test]
    fn test_parse_escaped_bytes_uppercase_hex() {
        assert_eq!(parse_escaped_bytes("\\xAF"), b"\xAF");
    }

    #[test]
    fn test_parse_escaped_bytes_zero_byte() {
        assert_eq!(parse_escaped_bytes("\\x00"), b"\x00");
    }

    #[test]
    fn test_parse_escaped_bytes_ff_byte() {
        assert_eq!(parse_escaped_bytes("\\xFF"), b"\xFF");
    }

    // ==================== parse_auto_mouse_at tests ====================

    #[test]
    fn test_parse_auto_mouse_at_move_only() {
        let (frame, evt) = parse_auto_mouse_at("100:160,90").unwrap();
        assert_eq!(frame, 100);
        assert_eq!(
            evt,
            AutoMouseEvent {
                x: 160,
                y: 90,
                buttons: None
            }
        );
    }

    #[test]
    fn test_parse_auto_mouse_at_with_buttons() {
        let (frame, evt) = parse_auto_mouse_at("120:10,20,1").unwrap();
        assert_eq!(frame, 120);
        assert_eq!(
            evt,
            AutoMouseEvent {
                x: 10,
                y: 20,
                buttons: Some(1)
            }
        );
    }

    #[test]
    fn test_parse_auto_mouse_at_hex_buttons() {
        let (frame, evt) = parse_auto_mouse_at("80:0x10,0x20,0x7").unwrap();
        assert_eq!(frame, 80);
        assert_eq!(
            evt,
            AutoMouseEvent {
                x: 0x10,
                y: 0x20,
                buttons: Some(0x07)
            }
        );
    }

    #[test]
    fn test_parse_auto_mouse_at_uppercase_hex_coords() {
        let (frame, evt) = parse_auto_mouse_at("80:0X10,0X20").unwrap();
        assert_eq!(frame, 80);
        assert_eq!(
            evt,
            AutoMouseEvent {
                x: 0x10,
                y: 0x20,
                buttons: None
            }
        );
    }

    #[test]
    fn test_parse_auto_mouse_at_masks_buttons() {
        let (frame, evt) = parse_auto_mouse_at("12:10,20,255").unwrap();
        assert_eq!(frame, 12);
        assert_eq!(evt.buttons, Some(0x07));
    }

    #[test]
    fn test_parse_auto_mouse_at_out_of_range() {
        let result = parse_auto_mouse_at("5:40000,0");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_too_many_fields() {
        let result = parse_auto_mouse_at("5:1,2,3,4");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_invalid_buttons() {
        let result = parse_auto_mouse_at("5:1,2,garbage");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_invalid_hex_buttons() {
        let result = parse_auto_mouse_at("5:1,2,0xGG");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_invalid_second_coord() {
        let result = parse_auto_mouse_at("5:1,garbage");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_invalid_hex_coord() {
        let result = parse_auto_mouse_at("5:0xGG,2");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_missing_colon() {
        let result = parse_auto_mouse_at("5,1,2");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_invalid_frame() {
        let result = parse_auto_mouse_at("nope:1,2");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_boundaries() {
        // Test i16::MIN (-32768) and i16::MAX (32767)
        let (frame, evt) = parse_auto_mouse_at("10:-32768,32767").unwrap();
        assert_eq!(frame, 10);
        assert_eq!(evt.x, i16::MIN);
        assert_eq!(evt.y, i16::MAX);

        // Test just outside range (should fail)
        assert!(parse_auto_mouse_at("10:-32769,0").is_err());
        assert!(parse_auto_mouse_at("10:32768,0").is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_missing_fields() {
        assert!(parse_auto_mouse_at("5:").is_err());
        assert!(parse_auto_mouse_at("5:1").is_err());
    }

    #[test]
    fn test_parse_auto_mouse_at_rejects_empty_fields_without_shifting() {
        for spec in ["5:,10,1", "5:10,,1", "5:10,20,"] {
            assert!(
                parse_auto_mouse_at(spec).is_err(),
                "frame parser accepted empty field in {spec}"
            );
            assert!(
                parse_auto_mouse_at_tick(spec).is_err(),
                "tick parser accepted empty field in {spec}"
            );
        }
    }

    // ==================== ascii_needs_shift tests ====================

    #[test]
    fn test_ascii_needs_shift_uppercase() {
        assert_eq!(ascii_needs_shift(b'A'), Some(b'a'));
        assert_eq!(ascii_needs_shift(b'Z'), Some(b'z'));
        assert_eq!(ascii_needs_shift(b'M'), Some(b'm'));
    }

    #[test]
    fn test_ascii_needs_shift_lowercase() {
        assert_eq!(ascii_needs_shift(b'a'), None);
        assert_eq!(ascii_needs_shift(b'z'), None);
    }

    #[test]
    fn test_ascii_needs_shift_numbers() {
        assert_eq!(ascii_needs_shift(b'0'), None);
        assert_eq!(ascii_needs_shift(b'9'), None);
    }

    #[test]
    fn test_ascii_needs_shift_symbols_on_number_row() {
        assert_eq!(ascii_needs_shift(b'!'), Some(b'1'));
        assert_eq!(ascii_needs_shift(b'@'), Some(b'2'));
        assert_eq!(ascii_needs_shift(b'#'), Some(b'3'));
        assert_eq!(ascii_needs_shift(b'$'), Some(b'4'));
        assert_eq!(ascii_needs_shift(b'%'), Some(b'5'));
        assert_eq!(ascii_needs_shift(b'^'), Some(b'6'));
        assert_eq!(ascii_needs_shift(b'&'), Some(b'7'));
        assert_eq!(ascii_needs_shift(b'*'), Some(b'8'));
        assert_eq!(ascii_needs_shift(b'('), Some(b'9'));
        assert_eq!(ascii_needs_shift(b')'), Some(b'0'));
    }

    #[test]
    fn test_ascii_needs_shift_other_shifted_symbols() {
        assert_eq!(ascii_needs_shift(b'_'), Some(b'-'));
        assert_eq!(ascii_needs_shift(b'+'), Some(b'='));
        assert_eq!(ascii_needs_shift(b'{'), Some(b'['));
        assert_eq!(ascii_needs_shift(b'}'), Some(b']'));
        assert_eq!(ascii_needs_shift(b'|'), Some(b'\\'));
        assert_eq!(ascii_needs_shift(b':'), Some(b';'));
        assert_eq!(ascii_needs_shift(b'"'), Some(b'\''));
        assert_eq!(ascii_needs_shift(b'<'), Some(b','));
        assert_eq!(ascii_needs_shift(b'>'), Some(b'.'));
        assert_eq!(ascii_needs_shift(b'?'), Some(b'/'));
        assert_eq!(ascii_needs_shift(b'~'), Some(b'`'));
    }

    #[test]
    fn test_ascii_needs_shift_unshifted_symbols() {
        assert_eq!(ascii_needs_shift(b'-'), None);
        assert_eq!(ascii_needs_shift(b'='), None);
        assert_eq!(ascii_needs_shift(b'['), None);
        assert_eq!(ascii_needs_shift(b']'), None);
        assert_eq!(ascii_needs_shift(b';'), None);
        assert_eq!(ascii_needs_shift(b'\''), None);
        assert_eq!(ascii_needs_shift(b','), None);
        assert_eq!(ascii_needs_shift(b'.'), None);
        assert_eq!(ascii_needs_shift(b'/'), None);
        assert_eq!(ascii_needs_shift(b'`'), None);
    }

    #[test]
    fn test_ascii_needs_shift_space() {
        assert_eq!(ascii_needs_shift(b' '), None);
    }

    #[test]
    fn test_ascii_needs_shift_control_chars() {
        assert_eq!(ascii_needs_shift(b'\n'), None);
        assert_eq!(ascii_needs_shift(b'\r'), None);
        assert_eq!(ascii_needs_shift(b'\t'), None);
    }

    // ==================== parse_auto_mouse_at_tick tests ====================

    #[test]
    fn test_parse_auto_mouse_at_tick_move_only() {
        let (tick, evt) = parse_auto_mouse_at_tick("120:7,8").unwrap();
        assert_eq!(tick, 120);
        assert_eq!(
            evt,
            AutoMouseEvent {
                x: 7,
                y: 8,
                buttons: None,
            }
        );
    }

    #[test]
    fn test_parse_auto_mouse_at_tick_with_buttons() {
        let (tick, evt) = parse_auto_mouse_at_tick("120:7,8,0x05").unwrap();
        assert_eq!(tick, 120);
        assert_eq!(
            evt,
            AutoMouseEvent {
                x: 7,
                y: 8,
                buttons: Some(0x05),
            }
        );
    }

    #[test]
    fn test_parse_auto_mouse_at_tick_too_many_fields() {
        assert!(parse_auto_mouse_at_tick("1:1,2,3,4").is_err());
    }
}
