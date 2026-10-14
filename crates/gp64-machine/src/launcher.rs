//! Drop-in defaults. Executable names belong only to the launcher.
use crate::DosMachine;
use anyhow::{bail, Result};
use std::path::Path;

pub const DEFAULT_BATCH: &str = "F1GP.BAT";
pub const DEFAULT_PROGRAM: &str = "GP.EXE";
pub const MISSING_DEFAULT: &str = "no F1GP.BAT or GP.EXE beside gp64";

pub fn default_program(folder: &Path) -> Result<&'static str> {
    let names: Vec<_> = std::fs::read_dir(folder)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name())
        .collect();
    let exists = |name: &str| {
        names
            .iter()
            .any(|entry| entry.to_string_lossy().eq_ignore_ascii_case(name))
    };
    let program = if exists(DEFAULT_BATCH) {
        DEFAULT_BATCH
    } else if exists(DEFAULT_PROGRAM) {
        DEFAULT_PROGRAM
    } else {
        bail!(MISSING_DEFAULT);
    };
    Ok(program)
}

pub fn launch_default(folder: &Path) -> Result<DosMachine> {
    DosMachine::launch(folder, default_program(folder)?, "")
}
