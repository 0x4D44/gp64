//! Embeds the application icon and version information in the Windows
//! executable. The resources are written as a compiled `.res` file, which the
//! MSVC linker (and lld-link) accept directly, so no resource compiler or
//! extra crate is needed. Other targets build without resources.

use std::{env, fs, path::PathBuf};

const ART: &str = "assets/icon.txt";
/// The group icon ID the frontend loads with `Icon::from_resource`.
const ICON_GROUP_ID: u16 = 1;
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
/// US English, the language the version strings are written in.
const LANGUAGE: u16 = 0x0409;
/// Unicode code page, paired with `LANGUAGE` in the version translation.
const CODE_PAGE: u16 = 1200;
/// Each icon size and the art it is scaled from by a whole multiple.
const ICON_SIZES: &[(u32, u32)] = &[(16, 16), (32, 32), (48, 16), (64, 32), (256, 32)];

fn main() {
    println!("cargo:rerun-if-changed={ART}");
    println!("cargo:rerun-if-changed=build.rs");
    let windows = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|target| target == "msvc");
    if !(windows && msvc) {
        return;
    }
    let art = fs::read_to_string(ART).unwrap_or_else(|error| panic!("read {ART}: {error}"));
    let images = parse_art(&art);
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("gp64.res");
    fs::write(&output, resources(&images)).expect("write gp64.res");
    println!("cargo:rustc-link-arg-bins={}", output.display());
}

/// A square RGBA image.
struct Image {
    size: u32,
    pixels: Vec<[u8; 4]>,
}

fn parse_art(text: &str) -> Vec<Image> {
    let mut palette = std::collections::HashMap::from([('.', [0, 0, 0, 0])]);
    let mut images: Vec<Image> = Vec::new();
    let mut rows_left = 0;
    for line in text.lines().map(str::trim_end) {
        if rows_left > 0 {
            let image = images.last_mut().unwrap();
            assert_eq!(line.chars().count() as u32, image.size, "{ART}: row width");
            for ch in line.chars() {
                let colour = palette.get(&ch);
                image
                    .pixels
                    .push(*colour.unwrap_or_else(|| panic!("{ART}: no palette entry for {ch:?}")));
            }
            rows_left -= 1;
        } else if let Some(size) = line.strip_prefix("size ") {
            let size: u32 = size.parse().expect("icon size");
            images.push(Image {
                size,
                pixels: Vec::new(),
            });
            rows_left = size;
        } else if let Some((ch, hex)) = line.split_once(' ') {
            if line.starts_with('#') {
                continue;
            }
            let mut chars = ch.chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else {
                panic!("{ART}: palette key must be one character: {line}");
            };
            let rgb = u32::from_str_radix(hex, 16).expect("palette colour");
            let [_, r, g, b] = rgb.to_be_bytes();
            palette.insert(ch, [r, g, b, 255]);
        }
    }
    assert_eq!(rows_left, 0, "{ART}: image is missing rows");
    images
}

/// Scales `art` by a whole multiple to `size` and encodes it as an icon
/// image: a 32-bit bottom-up DIB followed by its 1-bit transparency mask.
fn icon_image(art: &Image, size: u32) -> Vec<u8> {
    assert_eq!(
        size % art.size,
        0,
        "icon size {size} is not a multiple of {}",
        art.size
    );
    let scale = size / art.size;
    let pixel = |x: u32, y: u32| art.pixels[((y / scale) * art.size + x / scale) as usize];
    let mask_stride = size.div_ceil(32) * 4;
    let mut data = Vec::new();
    // BITMAPINFOHEADER; the height counts both the colour and mask planes.
    data.extend(40u32.to_le_bytes());
    data.extend(size.to_le_bytes());
    data.extend((size * 2).to_le_bytes());
    data.extend(1u16.to_le_bytes());
    data.extend(32u16.to_le_bytes());
    data.extend(0u32.to_le_bytes()); // BI_RGB
    data.extend((size * size * 4 + mask_stride * size).to_le_bytes());
    data.extend([0; 16]);
    for y in (0..size).rev() {
        for x in 0..size {
            let [r, g, b, a] = pixel(x, y);
            data.extend([b, g, r, a]);
        }
    }
    for y in (0..size).rev() {
        let mut row = vec![0u8; mask_stride as usize];
        for x in 0..size {
            if pixel(x, y)[3] == 0 {
                row[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        data.extend(row);
    }
    data
}

fn resources(images: &[Image]) -> Vec<u8> {
    let art = |size: u32| {
        images
            .iter()
            .find(|image| image.size == size)
            .unwrap_or_else(|| panic!("{ART}: no {size} x {size} image"))
    };
    // A .res file starts with an empty entry that marks its 32-bit format.
    let mut res = entry(0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((ICON_SIZES.len() as u16).to_le_bytes());
    for (id, &(size, from)) in (1u16..).zip(ICON_SIZES) {
        let image = icon_image(art(from), size);
        // GRPICONDIRENTRY: a 256-pixel dimension is stored as 0.
        group.extend([size as u8, size as u8, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((image.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
        res.extend(entry(RT_ICON, id, 0x1010, &image));
    }
    res.extend(entry(RT_GROUP_ICON, ICON_GROUP_ID, 0x1030, &group));
    res.extend(entry(RT_VERSION, 1, 0x0030, &version_info()));
    res
}

/// One resource entry with ordinal type and name, padded to a DWORD.
fn entry(kind: u16, name: u16, flags: u16, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    out.extend(
        [0xFFFF, kind, 0xFFFF, name]
            .into_iter()
            .flat_map(u16::to_le_bytes),
    );
    out.extend(0u32.to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(if kind == 0 { 0 } else { LANGUAGE }.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(data);
    pad(&mut out);
    out
}

fn pad(data: &mut Vec<u8>) {
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// A version-information block: header, key, value, then aligned children.
/// `value_length` counts words for text values and bytes for binary ones.
fn block(
    key: &str,
    value: &[u8],
    value_length: usize,
    text: bool,
    children: &[Vec<u8>],
) -> Vec<u8> {
    let mut out = vec![0; 4];
    out.extend(u16::from(text).to_le_bytes());
    out.extend(utf16z(key));
    pad(&mut out);
    out.extend(value);
    for child in children {
        pad(&mut out);
        out.extend(child);
    }
    let length = out.len() as u16;
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out[2..4].copy_from_slice(&(value_length as u16).to_le_bytes());
    out
}

fn version_info() -> Vec<u8> {
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let part = |name: &str| -> u32 { env::var(name).unwrap().parse().unwrap() };
    let (major, minor, patch) = (
        part("CARGO_PKG_VERSION_MAJOR"),
        part("CARGO_PKG_VERSION_MINOR"),
        part("CARGO_PKG_VERSION_PATCH"),
    );
    let prerelease = !env::var("CARGO_PKG_VERSION_PRE").unwrap().is_empty();
    let mut fixed = Vec::new();
    for word in [
        0xFEEF_04BD,           // signature
        0x0001_0000,           // structure version
        (major << 16) | minor, // file version
        patch << 16,
        (major << 16) | minor, // product version
        patch << 16,
        0x3F,                             // valid flag bits
        if prerelease { 0x2 } else { 0 }, // VS_FF_PRERELEASE
        0x0004_0004,                      // VOS_NT_WINDOWS32
        1,                                // VFT_APP
        0,
        0,
        0,
    ] {
        fixed.extend(u32::to_le_bytes(word));
    }
    let license = env::var("CARGO_PKG_LICENSE").unwrap();
    let strings = [
        ("FileDescription", "gp64".to_owned()),
        ("FileVersion", version.clone()),
        ("InternalName", "gp64".to_owned()),
        (
            "LegalCopyright",
            format!("Copyright (C) 2026 0x4D44. {license}."),
        ),
        ("OriginalFilename", "gp64.exe".to_owned()),
        ("ProductName", "gp64".to_owned()),
        ("ProductVersion", version),
    ]
    .map(|(key, value)| {
        let words = value.encode_utf16().count() + 1;
        block(key, &utf16z(&value), words, true, &[])
    });
    let table = block(
        &format!("{LANGUAGE:04X}{CODE_PAGE:04X}"),
        &[],
        0,
        true,
        &strings,
    );
    let string_info = block("StringFileInfo", &[], 0, true, &[table]);
    let translation: Vec<u8> = [LANGUAGE, CODE_PAGE]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect();
    let var = block("Translation", &translation, translation.len(), false, &[]);
    let var_info = block("VarFileInfo", &[], 0, true, &[var]);
    block(
        "VS_VERSION_INFO",
        &fixed,
        fixed.len(),
        false,
        &[string_info, var_info],
    )
}
