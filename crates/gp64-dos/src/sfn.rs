//! DOS 8.3 Short File Name (SFN) generation
//!
//! Implements the Windows FAT SFN generation algorithm:
//! - Valid 8.3 names pass through uppercased
//! - Long names are truncated to 6 chars + `~N` suffix
//! - Collision avoidance via incrementing suffix or hex hash fallback

use std::collections::HashSet;
use std::path::Path;

/// Characters valid in a DOS 8.3 filename (excluding dot, which is the separator).
/// A-Z, 0-9, and: ! # $ % & ' ( ) - @ ^ _ ` { } ~
fn is_valid_dos_char(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z' | '0'..='9' |
        '!' | '#' | '$' | '%' | '&' | '\'' | '(' | ')' |
        '-' | '@' | '^' | '_' | '`' | '{' | '}' | '~'
    )
}

/// Returns true if `name` is a valid DOS 8.3 filename.
///
/// Rules:
/// - Base: 1-8 characters
/// - Extension: 0-3 characters (optional, separated by single dot)
/// - Valid chars: A-Z, 0-9, ! # $ % & ' ( ) - @ ^ _ ` { } ~
/// - No spaces, no leading dot
/// - Case-insensitive (we check after uppercasing)
/// - Special entries `.` and `..` are valid
pub fn is_valid_dos_filename(name: &str) -> bool {
    // Special directory entries
    if name == "." || name == ".." {
        return true;
    }

    // Empty name is invalid
    if name.is_empty() {
        return false;
    }

    // No leading dot
    if name.starts_with('.') {
        return false;
    }

    // Split on dot
    let (base, ext) = if let Some(dot_pos) = name.find('.') {
        // Only one dot allowed
        if name[dot_pos + 1..].contains('.') {
            return false;
        }
        (&name[..dot_pos], &name[dot_pos + 1..])
    } else {
        (name, "")
    };

    // Base: 1-8 chars
    if base.is_empty() || base.len() > 8 {
        return false;
    }

    // Extension: 0-3 chars
    if ext.len() > 3 {
        return false;
    }

    // All chars must be valid DOS chars (case-insensitive)
    let all_valid = base.chars().all(is_valid_dos_char) && ext.chars().all(is_valid_dos_char);

    all_valid
}

/// Generate an 8.3 short filename from a long filename.
///
/// Algorithm (follows Windows FAT SFN generation):
/// 1. If already valid 8.3, return uppercased
/// 2. Strip invalid DOS characters
/// 3. Split on last `.` for base and extension
/// 4. Truncate extension to 3 chars, uppercase
/// 5. Truncate base to 6 chars, uppercase, append `~1`
/// 6. On collision, try `~2`, `~3`, `~4`
/// 7. Hash fallback: 4 chars + 2 hex + `~N`, probing until unused
pub fn generate_short_name<S: std::hash::BuildHasher>(
    long_name: &str,
    existing: &HashSet<String, S>,
) -> String {
    // Special entries pass through
    if long_name == "." || long_name == ".." {
        return long_name.to_string();
    }

    // If already valid 8.3, just uppercase it
    if is_valid_dos_filename(long_name) {
        return long_name.to_uppercase();
    }

    // Strip characters invalid in DOS filenames
    let stripped: String = long_name
        .chars()
        .filter(|&c| !matches!(c, ' ' | '+' | ',' | ';' | '=' | '[' | ']') && (c as u32) < 128)
        .collect();

    // Split on last dot for base and extension
    let (raw_base, raw_ext) = if let Some(dot_pos) = stripped.rfind('.') {
        (&stripped[..dot_pos], &stripped[dot_pos + 1..])
    } else {
        (stripped.as_str(), "")
    };

    // Strip leading dots from base
    let raw_base = raw_base.trim_start_matches('.');

    // Filter base and ext to only valid DOS chars
    let base_clean: String = raw_base.chars().filter(|c| is_valid_dos_char(*c)).collect();
    let ext_clean: String = raw_ext.chars().filter(|c| is_valid_dos_char(*c)).collect();

    // Uppercase and truncate extension to 3 chars
    let ext_upper: String = ext_clean.to_uppercase().chars().take(3).collect();

    // Truncate base to 6 chars, uppercase
    let base_upper: String = base_clean.to_uppercase().chars().take(6).collect();

    // Handle empty base after stripping
    let base_upper = if base_upper.is_empty() {
        "FILE".to_string()
    } else {
        base_upper
    };

    // Try ~1 through ~4
    for n in 1..=4 {
        let candidate = format_sfn(&base_upper, n, &ext_upper);
        if !existing.contains(&candidate) {
            return candidate;
        }
    }

    // Hash fallback: truncate base to 4 chars + 2 hex hash + ~N. Probe
    // every one-digit suffix before advancing the hash byte so an existing
    // real/native alias can never be returned as a generated alias.
    let initial_hash = (simple_hash(long_name) & 0xFF) as u8;
    let short_base: String = base_upper.chars().take(4).collect();
    for hash_offset in 0..=u8::MAX {
        let hash_hex = format!("{:02X}", initial_hash.wrapping_add(hash_offset));
        for n in 1..=9 {
            let candidate = if ext_upper.is_empty() {
                format!("{short_base}{hash_hex}~{n}")
            } else {
                format!("{short_base}{hash_hex}~{n}.{ext_upper}")
            };
            if !existing.contains(&candidate) {
                return candidate;
            }
        }
    }

    // A directory can reserve every hash variant. Continue through the legal
    // numeric namespace rather than returning a duplicate. The upper bound is
    // the largest suffix that still fits in an eight-character DOS base.
    for n in 5..=9_999_999 {
        let candidate = format_sfn(&base_upper, n, &ext_upper);
        if !existing.contains(&candidate) {
            return candidate;
        }
    }

    panic!("DOS short-name namespace exhausted for {long_name:?}")
}

/// Generate the non-colliding form required by INT 21h/71A8h.
///
/// The DOS long-filename API deliberately does not append a numeric tail.  It applies the
/// same character filtering as ordinary short-name generation, then truncates
/// the base and extension to the 8.3 fields.  Directory enumeration must use
/// [`generate_short_name`] instead, because it has the directory's collision
/// set available.
pub fn generate_short_name_without_tail(long_name: &str) -> Option<String> {
    if long_name == "." || long_name == ".." {
        return Some(long_name.to_string());
    }
    if long_name.is_empty() {
        return None;
    }
    if is_valid_dos_filename(long_name) {
        return Some(long_name.to_uppercase());
    }

    let stripped: String = long_name
        .chars()
        .filter(|&c| !matches!(c, ' ' | '+' | ',' | ';' | '=' | '[' | ']') && (c as u32) < 128)
        .collect();
    let (raw_base, raw_ext) = if let Some(dot_pos) = stripped.rfind('.') {
        (&stripped[..dot_pos], &stripped[dot_pos + 1..])
    } else {
        (stripped.as_str(), "")
    };
    let raw_base = raw_base.trim_start_matches('.');
    let base_clean: String = raw_base.chars().filter(|c| is_valid_dos_char(*c)).collect();
    let ext_clean: String = raw_ext.chars().filter(|c| is_valid_dos_char(*c)).collect();
    let base: String = base_clean.to_uppercase().chars().take(8).collect();
    let ext: String = ext_clean.to_uppercase().chars().take(3).collect();
    let base = if base.is_empty() {
        "FILE".to_string()
    } else {
        base
    };

    Some(if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    })
}

/// Assign deterministic DOS-visible short names for a directory listing.
///
/// Real FAT assigns short names from directory state. Host-backed directories do
/// not give us that state, so we use a stable ordering and reserve all real 8.3
/// names before generating aliases for long names. This prevents a generated
/// alias from colliding with a real short-name sibling.
pub fn assign_short_names(names: &[String]) -> Vec<(String, String)> {
    assign_short_names_with_preferred(names, &std::collections::HashMap::new())
}

fn assign_short_names_with_preferred(
    names: &[String],
    preferred: &std::collections::HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut ordered = names.to_vec();
    ordered.sort_by(|a, b| {
        a.to_uppercase()
            .cmp(&b.to_uppercase())
            .then_with(|| a.cmp(b))
    });

    let mut used_names: HashSet<String> = ordered
        .iter()
        .filter(|name| is_valid_dos_filename(name))
        .map(|name| name.to_uppercase())
        .collect();
    used_names.extend(
        preferred
            .values()
            .filter(|name| is_valid_dos_filename(name))
            .map(|name| name.to_uppercase()),
    );

    ordered
        .into_iter()
        .map(|name| {
            let short = if is_valid_dos_filename(&name) {
                name.to_uppercase()
            } else if let Some(short) = preferred
                .get(&name)
                .filter(|short| is_valid_dos_filename(short))
            {
                short.to_uppercase()
            } else {
                generate_short_name(&name, &used_names)
            };
            used_names.insert(short.clone());
            (name, short)
        })
        .collect()
}

#[cfg(windows)]
fn normalize_ntfs_short_name_component(component: &str) -> Option<String> {
    is_valid_dos_filename(component).then(|| component.to_uppercase())
}

/// On Windows, the directory/file's REAL NTFS 8.3 short name (final path
/// component) via `GetShortPathNameW`, upper-cased to the DOS convention.
///
/// The emulator's own [`assign_short_names`] builds an internal 8.3 namespace
/// (sort order + `~N`) that can DIVERGE from NTFS (creation order + hash) when
/// long sibling names collide in the 6-char prefix. Prefer the native alias when
/// one exists. On volumes with 8.3 generation disabled, however,
/// `GetShortPathNameW` reports success but returns the unchanged long component;
/// that is not DOS-visible 8.3 and must return `None` so callers fall back to the
/// internal namespace that `HostDrive` can resolve. See
/// `wrk_docs/2026.06.14 - HLD - DOS 8.3 short-name divergence fix`.
#[cfg(windows)]
pub fn ntfs_short_name_component(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    extern "system" {
        fn GetShortPathNameW(lpsz_long: *const u16, lpsz_short: *mut u16, cch: u32) -> u32;
    }
    let full = path.canonicalize().ok()?;
    let wide: Vec<u16> = full
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), needed) };
    if written == 0 || written as usize >= buf.len() {
        return None;
    }
    buf.truncate(written as usize);
    let short_full = String::from_utf16(&buf).ok()?;
    Path::new(&short_full)
        .file_name()
        .and_then(|c| c.to_str())
        .and_then(normalize_ntfs_short_name_component)
}

/// Assign DOS 8.3 short names for a set of sibling entry names.
///
/// On Windows, when the host directory is known, names that would be shortened
/// use the real NTFS alias when one exists. Native aliases and real 8.3 sibling
/// names are reserved before deterministic aliases are generated for entries on
/// 8.3-disabled parts of a mixed directory. Everywhere else (non-Windows, or
/// image/virtual drives with no host directory) this is exactly
/// [`assign_short_names`].
pub fn assign_short_names_in_dir(
    host_dir: Option<&Path>,
    names: &[String],
) -> Vec<(String, String)> {
    #[cfg(windows)]
    if let Some(dir) = host_dir {
        let preferred: std::collections::HashMap<_, _> = names
            .iter()
            .filter(|name| !is_valid_dos_filename(name))
            .filter_map(|name| {
                ntfs_short_name_component(&dir.join(name)).map(|short| (name.clone(), short))
            })
            .collect();
        return assign_short_names_with_preferred(names, &preferred);
    }
    let _ = host_dir;
    assign_short_names(names)
}

/// Format an 8.3 name from base + tilde number + extension.
fn format_sfn(base: &str, n: u32, ext: &str) -> String {
    let suffix = format!("~{n}");
    // Base + suffix must fit in 8 chars
    let max_base = 8 - suffix.len();
    let truncated_base: String = base.chars().take(max_base).collect();
    if ext.is_empty() {
        format!("{truncated_base}{suffix}")
    } else {
        format!("{truncated_base}{suffix}.{ext}")
    }
}

/// Simple hash function for fallback collision resolution.
fn simple_hash(s: &str) -> u32 {
    let mut hash: u32 = 0;
    for byte in s.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u32);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // is_valid_dos_filename tests
    // ========================================================================

    #[test]
    fn test_valid_simple_names() {
        assert!(is_valid_dos_filename("FILE.TXT"));
        assert!(is_valid_dos_filename("TEST.EXE"));
        assert!(is_valid_dos_filename("README"));
        assert!(is_valid_dos_filename("A.B"));
        assert!(is_valid_dos_filename("12345678.123"));
    }

    #[test]
    fn test_valid_special_chars() {
        assert!(is_valid_dos_filename("MY_FILE.TXT"));
        assert!(is_valid_dos_filename("FILE-1.DAT"));
        assert!(is_valid_dos_filename("HELLO!.TXT"));
        assert!(is_valid_dos_filename("A#B$C%D.E"));
        assert!(is_valid_dos_filename("F&G'H(I.J"));
        assert!(is_valid_dos_filename("A)B-C@D.^E"));
        assert!(is_valid_dos_filename("_`{}.~AB"));
    }

    #[test]
    fn test_valid_case_insensitive() {
        assert!(is_valid_dos_filename("file.txt"));
        assert!(is_valid_dos_filename("FiLe.TxT"));
        assert!(is_valid_dos_filename("test.exe"));
    }

    #[test]
    fn test_valid_dot_entries() {
        assert!(is_valid_dos_filename("."));
        assert!(is_valid_dos_filename(".."));
    }

    #[test]
    fn test_valid_no_extension() {
        assert!(is_valid_dos_filename("FILENAME"));
        assert!(is_valid_dos_filename("A"));
        assert!(is_valid_dos_filename("12345678"));
    }

    #[test]
    fn test_valid_boundary_lengths() {
        // Exactly 8 base chars
        assert!(is_valid_dos_filename("ABCDEFGH"));
        assert!(is_valid_dos_filename("ABCDEFGH.TXT"));
        // Exactly 3 ext chars
        assert!(is_valid_dos_filename("FILE.ABC"));
        // Max: 8.3
        assert!(is_valid_dos_filename("12345678.ABC"));
    }

    #[test]
    fn test_invalid_empty() {
        assert!(!is_valid_dos_filename(""));
    }

    #[test]
    fn test_invalid_too_long_base() {
        assert!(!is_valid_dos_filename("ABCDEFGHI"));
        assert!(!is_valid_dos_filename("ABCDEFGHI.TXT"));
        assert!(!is_valid_dos_filename("LONGFILENAME"));
    }

    #[test]
    fn test_invalid_too_long_ext() {
        assert!(!is_valid_dos_filename("FILE.ABCD"));
        assert!(!is_valid_dos_filename("FILE.TEXT"));
    }

    #[test]
    fn test_invalid_spaces() {
        assert!(!is_valid_dos_filename("MY FILE.TXT"));
        assert!(!is_valid_dos_filename("FILE .TXT"));
        assert!(!is_valid_dos_filename("FILE. TX"));
    }

    #[test]
    fn test_invalid_leading_dot() {
        assert!(!is_valid_dos_filename(".hidden"));
        assert!(!is_valid_dos_filename(".config"));
    }

    #[test]
    fn test_invalid_multiple_dots() {
        assert!(!is_valid_dos_filename("FILE.NAME.TXT"));
        assert!(!is_valid_dos_filename("A.B.C"));
    }

    #[test]
    fn test_invalid_chars() {
        assert!(!is_valid_dos_filename("FILE+1.TXT"));
        assert!(!is_valid_dos_filename("FILE,1.TXT"));
        assert!(!is_valid_dos_filename("FILE;1.TXT"));
        assert!(!is_valid_dos_filename("FILE=1.TXT"));
        assert!(!is_valid_dos_filename("FILE[1.TXT"));
        assert!(!is_valid_dos_filename("FILE]1.TXT"));
    }

    #[test]
    fn test_invalid_empty_base_with_ext() {
        // A dot with just extension but no base (not . or ..)
        assert!(!is_valid_dos_filename(".TXT"));
    }

    // ========================================================================
    // generate_short_name tests
    // ========================================================================

    #[test]
    fn test_gen_already_valid() {
        let existing = HashSet::new();
        assert_eq!(generate_short_name("FILE.TXT", &existing), "FILE.TXT");
        assert_eq!(generate_short_name("test.exe", &existing), "TEST.EXE");
        assert_eq!(generate_short_name("README", &existing), "README");
    }

    #[test]
    fn test_gen_special_entries() {
        let existing = HashSet::new();
        assert_eq!(generate_short_name(".", &existing), ".");
        assert_eq!(generate_short_name("..", &existing), "..");
    }

    #[test]
    fn test_gen_long_name_truncation() {
        let existing = HashSet::new();
        // "LONGFILENAME.TXT" -> base "LONGFI" + "~1" + ".TXT"
        assert_eq!(
            generate_short_name("LONGFILENAME.TXT", &existing),
            "LONGFI~1.TXT"
        );
    }

    #[test]
    fn test_gen_long_extension() {
        let existing = HashSet::new();
        // Long extension gets truncated to 3
        assert_eq!(
            generate_short_name("FILE.EXTENSION", &existing),
            "FILE~1.EXT"
        );
    }

    #[test]
    fn test_gen_strip_invalid_chars() {
        let existing = HashSet::new();
        // Spaces and special chars stripped
        assert_eq!(
            generate_short_name("MY FILE.TXT", &existing),
            "MYFILE~1.TXT"
        );
        assert_eq!(
            generate_short_name("FILE+NAME.TXT", &existing),
            "FILENA~1.TXT"
        );
    }

    #[test]
    fn test_gen_collision_avoidance() {
        let mut existing = HashSet::new();
        existing.insert("LONGFI~1.TXT".to_string());

        assert_eq!(
            generate_short_name("LONGFILENAME.TXT", &existing),
            "LONGFI~2.TXT"
        );
    }

    #[test]
    fn test_gen_collision_multiple() {
        let mut existing = HashSet::new();
        existing.insert("LONGFI~1.TXT".to_string());
        existing.insert("LONGFI~2.TXT".to_string());
        existing.insert("LONGFI~3.TXT".to_string());

        assert_eq!(
            generate_short_name("LONGFILENAME.TXT", &existing),
            "LONGFI~4.TXT"
        );
    }

    #[test]
    fn test_gen_collision_hash_fallback() {
        let mut existing = HashSet::new();
        existing.insert("LONGFI~1.TXT".to_string());
        existing.insert("LONGFI~2.TXT".to_string());
        existing.insert("LONGFI~3.TXT".to_string());
        existing.insert("LONGFI~4.TXT".to_string());

        let result = generate_short_name("LONGFILENAME.TXT", &existing);
        // Should be 4 chars + 2 hex + ~1 + .TXT
        assert!(result.ends_with(".TXT"), "got: {result}");
        assert!(result.contains("~1"), "got: {result}");
        // Base part (before .TXT) should be exactly 8 chars: 4 + 2hex + ~1
        let base = result.split('.').next().unwrap();
        assert_eq!(base.len(), 8, "base should be 8 chars, got: {base}");
        // Should not collide with existing
        assert!(!existing.contains(&result));
    }

    #[test]
    fn test_gen_hash_fallback_avoids_reserved_alias() {
        let long_name = "LONGFILENAME.TXT";
        let mut existing = HashSet::from([
            "LONGFI~1.TXT".to_string(),
            "LONGFI~2.TXT".to_string(),
            "LONGFI~3.TXT".to_string(),
            "LONGFI~4.TXT".to_string(),
        ]);
        let reserved_alias = generate_short_name(long_name, &existing);
        existing.insert(reserved_alias.clone());

        let generated = generate_short_name(long_name, &existing);

        assert!(is_valid_dos_filename(&generated));
        assert_ne!(generated, reserved_alias);
        assert!(!existing.contains(&generated));
    }

    #[test]
    fn test_gen_hash_fallback_uses_numeric_alias_after_hash_namespace_full() {
        let long_name = "LONGFILENAME.TXT";
        let mut existing = HashSet::from([
            "LONGFI~1.TXT".to_string(),
            "LONGFI~2.TXT".to_string(),
            "LONGFI~3.TXT".to_string(),
            "LONGFI~4.TXT".to_string(),
        ]);
        let initial_hash = (simple_hash(long_name) & 0xFF) as u8;
        for hash_offset in 0..=u8::MAX {
            let hash_hex = format!("{:02X}", initial_hash.wrapping_add(hash_offset));
            for n in 1..=9 {
                existing.insert(format!("LONG{hash_hex}~{n}.TXT"));
            }
        }

        let generated = generate_short_name(long_name, &existing);

        assert_eq!(generated, "LONGFI~5.TXT");
        assert!(!existing.contains(&generated));
    }

    #[test]
    fn test_gen_no_extension() {
        let existing = HashSet::new();
        assert_eq!(generate_short_name("LONGFILENAME", &existing), "LONGFI~1");
    }

    #[test]
    fn test_gen_multiple_dots() {
        let existing = HashSet::new();
        // Split on last dot: base="FILE.NAME", ext="TXT"
        // After stripping dots from base: "FILENA"
        let result = generate_short_name("FILE.NAME.TXT", &existing);
        assert!(result.ends_with(".TXT"), "got: {result}");
        assert!(result.contains("~1"), "got: {result}");
    }

    #[test]
    fn test_gen_lowercase_uppercased() {
        let existing = HashSet::new();
        assert_eq!(
            generate_short_name("longfilename.txt", &existing),
            "LONGFI~1.TXT"
        );
    }

    #[test]
    fn test_gen_leading_dot_file() {
        let existing = HashSet::new();
        // Leading dot is stripped, then it should generate a valid name
        let result = generate_short_name(".hidden", &existing);
        assert!(
            is_valid_dos_filename(&result),
            "result should be valid 8.3: {result}"
        );
        assert!(result.contains("~1"), "got: {result}");
    }

    #[test]
    fn test_gen_spaces_only_name() {
        let existing = HashSet::new();
        // All spaces stripped, falls back to "FILE"
        let result = generate_short_name("   .TXT", &existing);
        assert!(
            is_valid_dos_filename(&result),
            "result should be valid 8.3: {result}"
        );
    }

    #[test]
    fn test_gen_high_ascii_stripped() {
        let existing = HashSet::new();
        let result = generate_short_name("caf\u{00e9}.txt", &existing);
        assert!(
            is_valid_dos_filename(&result),
            "result should be valid 8.3: {result}"
        );
    }

    #[test]
    fn test_format_sfn_helper() {
        assert_eq!(format_sfn("LONGFI", 1, "TXT"), "LONGFI~1.TXT");
        assert_eq!(format_sfn("LONGFI", 2, "TXT"), "LONGFI~2.TXT");
        assert_eq!(format_sfn("LONGFI", 1, ""), "LONGFI~1");
        assert_eq!(format_sfn("AB", 1, "C"), "AB~1.C");
    }

    #[test]
    fn test_assign_short_names_reserves_real_8dot3_sibling() {
        let names = vec![
            "Sample_Project_With_A_Long_Directory_Name".to_string(),
            "SAMPLE~1".to_string(),
        ];
        let assigned = assign_short_names(&names);
        assert_eq!(
            assigned
                .iter()
                .find(|(name, _)| name == "SAMPLE~1")
                .map(|(_, short)| short.as_str()),
            Some("SAMPLE~1")
        );
        assert_eq!(
            assigned
                .iter()
                .find(|(name, _)| name == "Sample_Project_With_A_Long_Directory_Name")
                .map(|(_, short)| short.as_str()),
            Some("SAMPLE~2")
        );
    }

    #[test]
    fn test_assign_short_names_reserves_hash_fallback_sibling() {
        let long_name = "LONGFILENAME.TXT".to_string();
        let standard_aliases = [
            "LONGFI~1.TXT".to_string(),
            "LONGFI~2.TXT".to_string(),
            "LONGFI~3.TXT".to_string(),
            "LONGFI~4.TXT".to_string(),
        ];
        let existing: HashSet<_> = standard_aliases.iter().cloned().collect();
        let reserved_alias = generate_short_name(&long_name, &existing);
        let mut names = Vec::from(standard_aliases);
        names.push(reserved_alias.clone());
        names.push(long_name.clone());

        let assigned = assign_short_names(&names);
        let by_short: std::collections::HashMap<_, _> = assigned
            .iter()
            .map(|(name, short)| (short.as_str(), name.as_str()))
            .collect();

        assert_eq!(by_short.len(), names.len(), "every alias must be unique");
        assert_eq!(
            by_short.get(reserved_alias.as_str()).copied(),
            Some(reserved_alias.as_str()),
            "the real 8.3 sibling must retain its name"
        );
        for (name, short) in &assigned {
            assert_eq!(
                by_short.get(short.as_str()).copied(),
                Some(name.as_str()),
                "alias {short} must resolve to {name}"
            );
        }
    }

    #[test]
    fn test_assign_short_names_is_input_order_independent() {
        let forward = vec![
            "SAMPLE~1".to_string(),
            "Sample_Project_With_A_Long_Directory_Name".to_string(),
        ];
        let reverse = vec![
            "Sample_Project_With_A_Long_Directory_Name".to_string(),
            "SAMPLE~1".to_string(),
        ];

        assert_eq!(assign_short_names(&forward), assign_short_names(&reverse));
    }

    #[test]
    fn test_assign_short_names_reserves_preferred_native_sibling_aliases() {
        let native = "Sample_Project_Original".to_string();
        let generated = "Sample_Project_Updated".to_string();
        let names = vec![native.clone(), generated.clone()];
        let preferred = std::collections::HashMap::from([(native.clone(), "SAMPLE~2".into())]);

        let assigned: std::collections::HashMap<_, _> =
            assign_short_names_with_preferred(&names, &preferred)
                .into_iter()
                .collect();
        assert_eq!(assigned.get(&native).map(String::as_str), Some("SAMPLE~2"));
        assert_eq!(
            assigned.get(&generated).map(String::as_str),
            Some("SAMPLE~1"),
            "generated alias must not collide with a sibling's native NTFS alias"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_ntfs_short_name_validation_precedes_unicode_uppercase() {
        assert_eq!(
            normalize_ntfs_short_name_component("SAMPLE~1"),
            Some("SAMPLE~1".into())
        );
        assert_eq!(
            normalize_ntfs_short_name_component("ß"),
            None,
            "an invalid passthrough component must not become the fictitious alias SS"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_ntfs_short_name_component_never_returns_a_long_name() {
        // Build on this checkout's volume instead of %TEMP%. On an NTFS volume
        // with 8.3 generation disabled, GetShortPathNameW reports success but
        // returns the unchanged long component. That is not a DOS short name
        // and must be reported as unavailable so callers use assign_short_names.
        let temp_dir = tempfile::Builder::new()
            .prefix("mdd46_ntfs_sfn_")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let long_dir = temp_dir
            .path()
            .join("Sample_Project_With_A_Long_Directory_Name");
        std::fs::create_dir(&long_dir).unwrap();

        if let Some(component) = ntfs_short_name_component(&long_dir) {
            assert!(
                is_valid_dos_filename(&component),
                "GetShortPathNameW passthrough is not a DOS 8.3 name: {component:?}"
            );
        }
    }

    #[test]
    fn test_is_valid_dos_char_coverage() {
        // Verify all listed valid chars
        for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ".chars() {
            assert!(is_valid_dos_char(c), "should be valid: {c}");
        }
        for c in "abcdefghijklmnopqrstuvwxyz".chars() {
            assert!(is_valid_dos_char(c), "should be valid: {c}");
        }
        for c in "0123456789".chars() {
            assert!(is_valid_dos_char(c), "should be valid: {c}");
        }
        for c in "!#$%&'()-@^_`{}~".chars() {
            assert!(is_valid_dos_char(c), "should be valid: {c}");
        }
        // Invalid chars
        for c in " +,;=[]<>|\\/:\"*?".chars() {
            assert!(!is_valid_dos_char(c), "should be invalid: {c}");
        }
    }

    #[test]
    fn test_simple_hash_deterministic() {
        assert_eq!(simple_hash("test"), simple_hash("test"));
        assert_ne!(simple_hash("test"), simple_hash("other"));
    }
}
