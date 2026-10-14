//! Source policy: every ignored test in this crate must declare its tier.
//!
//! `MDD-BUG-CRUCIBLE-00521`. A bare `#[ignore]` sends a test **nowhere**:
//! ordinary `cargo test` skips it because it is ignored, and `full_test` skips
//! it too because `src/ignore_tiers.rs` admits a test to the nightly sweep only
//! when its reason string contains the literal
//! `full_test (--include-ignored)`. Twenty-eight modem tests were in that state,
//! including suites whose own comments assigned them to bounded automation.
//!
//! The reason string is therefore the tier declaration, and this guard makes
//! writing one mandatory. It deliberately does not judge *which* tier is right —
//! that is a human call recorded in the reason text — only that a choice was
//! made and written down.

use std::path::{Path, PathBuf};

/// Files under these directories are scanned.
const SCANNED_DIRS: [&str; 2] = ["src", "tests"];

/// The marker `full_test` looks for when deciding whether to run an ignored
/// test in the nightly sweep. Duplicated here as a literal on purpose: if the
/// policy string ever changes, this guard should be updated deliberately rather
/// than silently following along.
const NIGHTLY_MARKER: &str = "full_test (--include-ignored)";

fn rust_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    for dir in SCANNED_DIRS {
        walk(&root.join(dir), &mut out);
    }
    out.sort();
    out
}

/// No `#[ignore]` without a reason.
#[test]
fn every_ignored_test_declares_a_tier() {
    let mut bare = Vec::new();

    for path in rust_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            // `#[ignore = "..."]` is the compliant form; `#[ignore]` — with or
            // without a trailing comment — is not, because libtest never sees a
            // comment.
            if trimmed.starts_with("#[ignore]") {
                bare.push(format!("{}:{}", path.display(), index + 1));
            }
        }
    }

    assert!(
        bare.is_empty(),
        "{} ignored test(s) carry no reason, so they run in NO tier — not the \
         default `cargo test` (ignored) and not the nightly sweep (no \
         `{NIGHTLY_MARKER}` marker). Give each one a reason that says where it \
         belongs: the marker for tests the nightly should run, or an explicit \
         justification (diagnostic, soak-only, quarantine, harness limitation) \
         for tests it should not.\n  {}",
        bare.len(),
        bare.join("\n  ")
    );
}

/// The guard above must be scanning something, or it passes vacuously.
#[test]
fn the_ignore_policy_scan_reaches_real_sources() {
    let sources = rust_sources();
    assert!(
        sources.len() > 10,
        "only {} source files found — the scan root is wrong and the policy \
         guard would pass vacuously",
        sources.len()
    );

    let ignored_total: usize = sources
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|text| {
            text.lines()
                .filter(|l| l.trim_start().starts_with("#[ignore"))
                .count()
        })
        .sum();
    assert!(
        ignored_total > 0,
        "no ignored tests found at all — either the crate changed shape or the \
         scan is broken"
    );
}
