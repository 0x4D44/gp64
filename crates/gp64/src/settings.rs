//! Menu choices kept between runs in the per-user configuration folder,
//! never in the program's folder. Command-line flags win for one run.
use crate::{
    cli::{self, Options},
    scaler::Scaler,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub scaler: Scaler,
    pub fullscreen: bool,
    pub mute: bool,
    /// Modem COM port (1 or 2) from the next launch; `None` keeps the default.
    pub com: Option<u8>,
    /// The last TCP port hosted from the menu.
    pub modem_host_port: Option<u16>,
    /// The last `host:port` joined from the menu. An address, never a secret.
    pub modem_join: Option<String>,
}
impl Settings {
    /// The saved host port, if it is a usable one.
    pub fn host_port(&self) -> Option<u16> {
        self.modem_host_port.filter(|&port| port != 0)
    }
    /// The saved join address, if it is a usable one.
    pub fn join(&self) -> Option<&str> {
        self.modem_join.as_deref().and_then(cli::parse_join)
    }
    /// The saved modem COM port, if it is a usable one.
    pub fn com(&self) -> Option<u8> {
        self.com.filter(|com| (1..=2).contains(com))
    }
    /// Applies saved choices that the command line left unset.
    pub fn apply(&self, options: &mut Options) {
        if !options.com_given {
            if let Some(com) = self.com() {
                options.com = com;
            }
        }
    }
}

/// `<config dir>/gp64/settings.json`, where the platform has a config dir.
pub fn path() -> Option<PathBuf> {
    gp64_types::host_dirs::config_dir().map(|dir| dir.join("gp64").join("settings.json"))
}

/// A missing file gives defaults silently; an unreadable one gives
/// defaults and one line on `stderr`.
pub fn load_from(path: &Path, stderr: &mut impl Write) -> Settings {
    let text = match std::fs::read(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(error) => {
            let _ = writeln!(
                stderr,
                "gp64: ignoring settings {}: {error}",
                path.display()
            );
            return Settings::default();
        }
    };
    match serde_json::from_slice(&text) {
        Ok(settings) => settings,
        Err(error) => {
            let _ = writeln!(
                stderr,
                "gp64: ignoring settings {}: {error}",
                path.display()
            );
            Settings::default()
        }
    }
}

/// Writes a temporary file beside the target, then renames it over the
/// target, so a crash never leaves a half-written file.
pub fn save_to(path: &Path, settings: &Settings) -> std::io::Result<()> {
    save_to_with_open_hook(path, settings, || {})
}

fn save_to_with_open_hook(
    path: &Path,
    settings: &Settings,
    after_temp_open: impl FnOnce(),
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    let (temporary, mut file) = create_temporary_file(path)?;
    after_temp_open();
    let written = file.write_all(&text).and_then(|()| file.sync_all());
    drop(file);
    let written = written.and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

fn create_temporary_file(path: &Path) -> std::io::Result<(PathBuf, File)> {
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
    loop {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(format!(".tmp.{}.{}", std::process::id(), sequence));
        let temporary = PathBuf::from(temporary);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Loads from [`path`]; defaults when the platform has no config dir.
pub fn load() -> Settings {
    path().map_or_else(Settings::default, |path| {
        load_from(&path, &mut std::io::stderr().lock())
    })
}

/// Saves to [`path`]; a failure costs only persistence, so it is reported
/// and otherwise ignored.
pub fn save(settings: &Settings) {
    if let Some(path) = path() {
        if let Err(error) = save_to(&path, settings) {
            eprintln!("gp64: cannot save settings {}: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{parse, Command};
    use std::{
        ffi::OsString,
        sync::atomic::{AtomicU64, Ordering},
        sync::mpsc,
        thread,
        time::Duration,
    };

    fn folder(name: &str) -> PathBuf {
        static NEXT_FOLDER: AtomicU64 = AtomicU64::new(0);
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("gp64 crate lives below the worktree root")
            .join(".scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        loop {
            let folder = scratch.join(format!(
                "gp64-settings-{name}-{}-{}",
                std::process::id(),
                NEXT_FOLDER.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&folder) {
                Ok(()) => return folder,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create settings test folder: {error}"),
            }
        }
    }
    fn options(args: &[&str]) -> Options {
        match parse(args.iter().map(std::ffi::OsString::from)).unwrap() {
            Command::Run(options) => options,
            _ => panic!(),
        }
    }

    #[test]
    fn settings_round_trip_through_an_atomic_write() {
        let folder = folder("round-trip");
        let path = folder.join("gp64").join("settings.json");
        let settings = Settings {
            scaler: Scaler::Smooth,
            fullscreen: true,
            mute: true,
            com: Some(1),
            modem_host_port: Some(2300),
            modem_join: Some("peer.example:2300".into()),
        };
        save_to(&path, &settings).unwrap();
        let mut stderr = Vec::new();
        assert_eq!(load_from(&path, &mut stderr), settings);
        assert!(stderr.is_empty());
        // Overwriting replaces the file and leaves no temporary behind.
        save_to(&path, &Settings::default()).unwrap();
        assert_eq!(load_from(&path, &mut stderr), Settings::default());
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["settings.json"]);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn overlapping_saves_publish_one_complete_document() {
        let folder = folder("overlap");
        let path = folder.join("settings.json");
        let longer = Settings {
            fullscreen: true,
            modem_join: Some(format!("{}:2300", "peer".repeat(1024))),
            ..Settings::default()
        };
        let shorter = Settings {
            scaler: Scaler::Smooth,
            mute: true,
            com: Some(1),
            modem_host_port: Some(2400),
            ..Settings::default()
        };
        let (short_opened_tx, short_opened_rx) = mpsc::channel();
        let (long_saved_tx, long_saved_rx) = mpsc::channel();

        let long_path = path.clone();
        let long_settings = longer.clone();
        let long_writer = thread::spawn(move || {
            let result = save_to_with_open_hook(&long_path, &long_settings, || {
                short_opened_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("short writer opened its temporary file");
            });
            let _ = long_saved_tx.send(());
            result
        });

        let short_path = path.clone();
        let short_settings = shorter.clone();
        let short_writer = thread::spawn(move || {
            save_to_with_open_hook(&short_path, &short_settings, || {
                short_opened_tx.send(()).unwrap();
                long_saved_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("long writer renamed its complete document");
            })
        });

        let long_result = long_writer.join().expect("long writer did not panic");
        let short_result = short_writer.join().expect("short writer did not panic");
        let bytes = std::fs::read(&path).unwrap();
        let parsed = serde_json::from_slice::<Settings>(&bytes);
        assert!(
            parsed.is_ok(),
            "overlapping saves must leave parseable JSON, got {parsed:?}"
        );
        let published = parsed.unwrap();
        assert!(
            published == longer || published == shorter,
            "published settings must equal one complete writer document, got {published:?}"
        );
        assert!(long_result.is_ok(), "long save failed: {long_result:?}");
        assert!(short_result.is_ok(), "short save failed: {short_result:?}");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn failed_replacement_preserves_foreign_temp_and_cleans_own_temp() {
        let folder = folder("cleanup");
        let path = folder.join("settings.json");
        let foreign_temp = folder.join("settings.json.tmp");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("marker"), b"target directory").unwrap();
        std::fs::write(&foreign_temp, b"another writer's temporary file").unwrap();

        assert!(save_to(&path, &Settings::default()).is_err());

        assert_eq!(
            std::fs::read(&foreign_temp).unwrap(),
            b"another writer's temporary file"
        );
        assert_eq!(
            std::fs::read(path.join("marker")).unwrap(),
            b"target directory"
        );
        let mut names: Vec<OsString> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["settings.json", "settings.json.tmp"]);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn missing_is_silent_and_corrupt_falls_back_with_one_line() {
        let folder = folder("corrupt");
        let path = folder.join("settings.json");
        let mut stderr = Vec::new();
        assert_eq!(load_from(&path, &mut stderr), Settings::default());
        assert!(stderr.is_empty());
        std::fs::create_dir_all(&folder).unwrap();
        for text in ["{not json", "{\"scaler\":\"sideways\"}", "42"] {
            std::fs::write(&path, text).unwrap();
            let mut stderr = Vec::new();
            assert_eq!(load_from(&path, &mut stderr), Settings::default());
            let stderr = String::from_utf8(stderr).unwrap();
            assert_eq!(stderr.lines().count(), 1, "{text}: {stderr}");
            assert!(stderr.starts_with("gp64: ignoring settings"));
        }
        // Unknown fields and missing fields are tolerated.
        std::fs::write(&path, "{\"mute\":true,\"later\":1}").unwrap();
        let loaded = load_from(&path, &mut stderr);
        assert!(loaded.mute && loaded.scaler == Scaler::Integer);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn every_saved_scaler_name_loads() {
        let folder = folder("scalers");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("settings.json");
        for (name, expected) in [
            ("integer", Scaler::Integer),
            ("smooth", Scaler::Smooth),
            ("area", Scaler::Area),
            ("crt", Scaler::Crt),
        ] {
            std::fs::write(&path, format!("{{\"scaler\":\"{name}\",\"mute\":true}}")).unwrap();
            let mut stderr = Vec::new();
            let loaded = load_from(&path, &mut stderr);
            assert_eq!(loaded.scaler, expected, "{name}");
            assert!(loaded.mute && stderr.is_empty());
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn old_saved_speed_is_ignored_for_automatic_cpu_pacing() {
        for speed in [0, 8, 33, 1000] {
            let saved: Settings =
                serde_json::from_str(&format!("{{\"speed_mhz\":{speed}}}")).unwrap();
            let mut plain = options(&[]);
            saved.apply(&mut plain);
            assert_eq!(plain, options(&[]));
            assert!(!serde_json::to_string(&saved).unwrap().contains("speed_mhz"));
        }
    }

    #[test]
    fn command_line_com_overrides_the_saved_com() {
        let saved = Settings {
            com: Some(1),
            ..Settings::default()
        };
        let mut plain = options(&[]);
        saved.apply(&mut plain);
        assert_eq!(plain.com, 1);
        let mut flagged = options(&["--com", "2"]);
        saved.apply(&mut flagged);
        assert_eq!(flagged.com, 2);
        for unusable in [Some(0), Some(3), None] {
            let mut default = options(&[]);
            Settings {
                com: unusable,
                ..Settings::default()
            }
            .apply(&mut default);
            assert_eq!(default.com, 2);
        }
    }

    #[test]
    fn unusable_saved_modem_values_are_ignored() {
        let saved = Settings {
            modem_host_port: Some(0),
            modem_join: Some("no-port".into()),
            ..Settings::default()
        };
        assert_eq!((saved.host_port(), saved.join()), (None, None));
        let saved = Settings {
            modem_host_port: Some(5000),
            modem_join: Some("h:1".into()),
            ..Settings::default()
        };
        assert_eq!((saved.host_port(), saved.join()), (Some(5000), Some("h:1")));
    }

    #[test]
    fn a_file_from_before_the_modem_settings_still_loads() {
        let folder = folder("older");
        let path = folder.join("settings.json");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            &path,
            "{\"scaler\":\"smooth\",\"fullscreen\":true,\"mute\":false,\"speed_mhz\":25}",
        )
        .unwrap();
        let mut stderr = Vec::new();
        let loaded = load_from(&path, &mut stderr);
        assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
        assert_eq!(
            loaded,
            Settings {
                scaler: Scaler::Smooth,
                fullscreen: true,
                ..Settings::default()
            }
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
