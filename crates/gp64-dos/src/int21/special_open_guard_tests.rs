//! MDD-BUG-CRUCIBLE-00375 guard: no synthetic-file branch of INT 21h/AH=3Dh
//! may allocate its handle through the sharing-less `FileTable::allocate`.
//!
//! `FileTable::allocate` hardcodes SHARE_COMPAT plus read access, so any open
//! routed through it silently discards the guest's requested access and deny
//! modes. The behavioural tests in `tests/dos/special_open_validation_tests.rs`
//! cover the registered-virtual-file branch, which a test can register itself.
//! when the corresponding assets resolve, so reverting either to bare
//! `allocate` fails no test — verified, and it did not.
//!
//! Rather than ship two untested edits this pins the shape. It is a guard in
//! the CLAUDE.md sense: its input is the repo itself. It lives in its own file
//! because a guard inside the file it scans counts its own string literals.

/// Strip ALL whitespace so a wrapped `self.file_table\n    .allocate(` reads
/// the same as the single-line form. The original review missed exactly one
/// site for this reason, and so did my first grep — collapsing to single
/// spaces is not enough, because Rust puts no space around `.` or `(`.
fn flattened_open_handler() -> String {
    include_str!("mod.rs")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

#[test]
fn no_int21_open_branch_uses_the_sharing_less_allocate_crucible375() {
    let flat = flattened_open_handler();

    // `allocate_with_sharing(` also contains `allocate`, so match the exact
    // call including its opening paren.
    let bare = flat.matches("file_table.allocate(").count();
    assert_eq!(
        bare, 0,
        "an INT 21h open branch allocates through FileTable::allocate, which \
         hardcodes SHARE_COMPAT and read access and therefore discards the \
         guest's requested access and deny modes"
    );

    // All three synthetic/device branches must go through a sharing-aware
    //
    // Two forms qualify. `allocate_synthetic_with_sharing` delegates to
    // `allocate_with_sharing` and additionally records how a snapshot restore
    // must rebuild the handle (MDD-BUG-CRUCIBLE-00374); the two branches
    // whose bytes never lived on a drive use it, and NUL — a device, not a
    // rebuildable payload — uses the plain form.
    let synthetic = flat
        .matches("file_table.allocate_synthetic_with_sharing(")
        .count();
    let plain = flat.matches("file_table.allocate_with_sharing(").count();
    assert_eq!(
        synthetic, 2,
        "expected the GLIDE2X.OVL, registered-virtual-file \
         branches to allocate through allocate_synthetic_with_sharing, so a \
         snapshot restore can rebuild them from their source"
    );
    assert_eq!(
        synthetic + plain,
        3,
        "expected exactly three sharing-aware allocations in the INT 21h open \
         path (NUL, GLIDE2X.OVL, registered virtual file); a \
         changed count means a branch was added or lost"
    );
}

/// The access-code check must run BEFORE any early return can happen — that
/// ordering is the defect, and a behavioural test cannot see where in the
/// function the check sits, only that it fired for the names it can reach.
#[test]
fn access_validation_precedes_every_early_return_crucible375() {
    let source = include_str!("mod.rs");

    let check = source
        .find("if dos_access_read_write(access_mode).is_none()")
        .expect("the hoisted access-code check must exist");

    // The first thing that can return early after AL is read.
    let first_early_return = source
        .find("if base_name == \"EMMXXXX0\"")
        .expect("the EMS early return must exist");

    assert!(
        check < first_early_return,
        "the access-code validation must precede the first early return; \
         placing it after them is what let intercepted names bypass it"
    );
}
