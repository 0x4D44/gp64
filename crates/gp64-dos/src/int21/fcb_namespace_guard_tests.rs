//! MDD-BUG-CRUCIBLE-00446 source-shape guard: the FCB wildcard handlers must
//! *match* on the DOS-visible name and *act* on the host name.
//!
//! Why a shape guard rather than more behavioural coverage: the "act on the
//! host name" half is not observable from a test on this host. Swapping the
//! source path back to the DOS alias still passes every behavioural test,
//! because `HostDrive::resolve_generated_sfn_path` re-derives the alias
//! assignment and maps `LONGFI~1.TXT` back to the host file, and Windows is
//! case-insensitive on top of that. The distinction is real on a
//! case-sensitive host, for a non-host-backed drive whose `resolve_host_path`
//! returns `None`, and for any alias the back-resolver's `contains('~')`
//! precondition rejects — so the shape is pinned here instead of left to a
//! mutation that legitimately fails nothing.
//!
//! This guard lives in its own file on purpose: a guard that scans the file it
//! is written in would count its own string literals.

/// The INT 21h dispatch source, read at compile time.
const INT21_SOURCE: &str = include_str!("mod.rs");

/// Collapse all whitespace so line wrapping and rustfmt cannot move a match.
fn squeezed(source: &str) -> String {
    source.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn fcb_wildcard_handlers_match_on_the_dos_name() {
    let source = squeezed(INT21_SOURCE);
    let matches = source
        .matches("matches_pattern_static(file_pat,&entry.dos_name)")
        .count();
    assert_eq!(
        matches, 2,
        "expected exactly two FCB wildcard match sites (delete AH=13h and \
         rename AH=17h) testing the pattern against the DOS-visible name; a \
         site testing `entry.host_name` would let the guest enumerate a file \
         it cannot then delete or rename (MDD-BUG-CRUCIBLE-00446)"
    );
    assert!(
        !source.contains("matches_pattern_static(file_pat,&entry.host_name)"),
        "an FCB wildcard pattern must never be tested against the raw host name"
    );
}

#[test]
fn fcb_wildcard_handlers_build_their_source_paths_from_the_host_name() {
    let source = squeezed(INT21_SOURCE);
    let bindings = source.matches("lethost_name=&entry.host_name;").count();
    assert_eq!(
        bindings, 2,
        "expected exactly two FCB wildcard sites (delete AH=13h and rename \
         AH=17h) to build the path they act on from the host name behind the \
         alias (MDD-BUG-CRUCIBLE-00446)"
    );
    let host_paths = source
        .matches("format!(\"{dir_part}\\\\{host_name}\")")
        .count();
    assert_eq!(
        host_paths, 2,
        "both FCB wildcard sites must spell their subdirectory source path \
         with the host name"
    );
    let root_paths = source.matches("format!(\"\\\\{host_name}\")").count();
    assert_eq!(
        root_paths, 2,
        "both FCB wildcard sites must spell their root source path with the \
         host name"
    );
}
