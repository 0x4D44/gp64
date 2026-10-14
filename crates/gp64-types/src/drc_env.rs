//! The portable DRC's two debug list variables, each read once per process.
//!
//! * `MDEM_DRC` — runtime controls. Each key CHANGES what the DRC emits or
//!   runs, by design, while guest-visible state stays equal:
//!   - `cyclic=<off|auto|control:<phys>+...|resident:<phys>+...>` — cyclic
//!     trace mode (default `auto`; a manifest joins its hex addresses with `+`)
//!   - `regions=<off|table|heat|form|form:<MiB>>` — region machinery mode
//!   - `cold_split=off` — keep baseline `b_cold` arms inline
//!   - `fastmem=<off|predemote|force_fault>` — fastmem JIT window arm
//!   - `no_x87` — keep every x87 instruction off the DRC
//!   - `arena=<bytes>` — AArch64 code-arena capacity
//! * `MDEM_DRC_DIAG` — probes. Each key only measures:
//!   - `census` — the diagnostic censuses
//!   - `census_hot_dump=<path>` — with `census`, write the hottest blocks here
//!   - `census_hot_blocks=<n>` — how many blocks that dump lists (default 10)
//!   - `perf_map` — the `/tmp/perf-<pid>.map` sampler map
//!   - `perf_map_retain` — keep released arena ranges mapped for the map
//!   - `rung0=<path>` — the Rung-0 image dump
//!   - `rung0_targets=<path>` — the address file that narrows that dump
//!
//! Syntax: comma-separated `key` or `key=value`; a repeated key's last value
//! wins. An unknown key prints one stderr warning naming the known keys, so a
//! typo cannot pass for a different experiment.

use std::sync::OnceLock;

/// Keys `MDEM_DRC` accepts.
pub const DRC_KEYS: &[&str] = &[
    "cyclic",
    "regions",
    "cold_split",
    "fastmem",
    "no_x87",
    "arena",
];

/// Keys `MDEM_DRC_DIAG` accepts.
pub const DRC_DIAG_KEYS: &[&str] = &[
    "census",
    "census_hot_dump",
    "census_hot_blocks",
    "link_census",
    "perf_map",
    "perf_map_retain",
    "rung0",
    "rung0_targets",
];

/// The value of `key` in a comma-separated `key[=value]` list: `Some("")` for
/// a bare key, the last value when the key repeats, `None` when absent.
#[must_use]
pub fn list_value<'a>(list: &'a str, key: &str) -> Option<&'a str> {
    list.split(',')
        .filter_map(|item| {
            let item = item.trim();
            let (name, value) = item.split_once('=').unwrap_or((item, ""));
            (name.trim() == key).then_some(value.trim())
        })
        .next_back()
}

/// Every key in `list` that `known` does not name, in list order.
#[must_use]
pub fn unknown_keys<'a>(list: &'a str, known: &[&str]) -> Vec<&'a str> {
    list.split(',')
        .map(|item| item.split_once('=').map_or(item, |(name, _)| name).trim())
        .filter(|name| !name.is_empty() && !known.contains(name))
        .collect()
}

fn checked(var: &str, read: Result<String, std::env::VarError>, known: &[&str]) -> String {
    match read {
        Ok(list) => {
            let unknown = unknown_keys(&list, known);
            if !unknown.is_empty() {
                eprintln!(
                    "[DRC] {var}: unknown key(s) {unknown:?} ignored (known: {})",
                    known.join(", ")
                );
            }
            list
        }
        Err(std::env::VarError::NotPresent) => String::new(),
        Err(std::env::VarError::NotUnicode(_)) => {
            eprintln!("[DRC] {var} is not valid UTF-8 and is ignored");
            String::new()
        }
    }
}

/// The value of `key` in `MDEM_DRC`, read and checked once per process.
#[must_use]
pub fn drc(key: &str) -> Option<&'static str> {
    static LIST: OnceLock<String> = OnceLock::new();
    let list = LIST.get_or_init(|| checked("MDEM_DRC", std::env::var("MDEM_DRC"), DRC_KEYS));
    list_value(list, key)
}

/// The value of `key` in `MDEM_DRC_DIAG`, read and checked once per process.
#[must_use]
pub fn drc_diag(key: &str) -> Option<&'static str> {
    static LIST: OnceLock<String> = OnceLock::new();
    let list = LIST.get_or_init(|| {
        checked(
            "MDEM_DRC_DIAG",
            std::env::var("MDEM_DRC_DIAG"),
            DRC_DIAG_KEYS,
        )
    });
    list_value(list, key)
}

/// Is the bare flag `key` set in `MDEM_DRC_DIAG`? `key=0` and `key=off` read
/// as unset.
#[must_use]
pub fn drc_diag_flag(key: &str) -> bool {
    drc_diag(key).is_some_and(|value| value != "0" && value != "off")
}

#[cfg(test)]
mod tests {
    use super::{list_value, unknown_keys, DRC_DIAG_KEYS, DRC_KEYS};

    #[test]
    fn list_parses_flags_values_repeats_and_unknown_keys() {
        let list = "cyclic=resident:alpha+beta, no_x87 ,arena=65536,arena=131072,bogus=1";
        assert_eq!(list_value(list, "cyclic"), Some("resident:alpha+beta"));
        assert_eq!(list_value(list, "no_x87"), Some(""));
        assert_eq!(list_value(list, "arena"), Some("131072"));
        assert_eq!(list_value(list, "fastmem"), None);
        assert_eq!(list_value("", "census"), None);
        assert_eq!(unknown_keys(list, DRC_KEYS), vec!["bogus"]);
        assert_eq!(
            unknown_keys("census,link_census,perf_map,rung0=/tmp/r0,x", DRC_DIAG_KEYS),
            vec!["x"]
        );
    }
}
