//! The command line is independent of window and device creation.
use anyhow::{bail, Context, Result};
use gp64_machine::launcher;
use std::{
    ffi::OsString,
    io::IsTerminal,
    path::{Path, PathBuf},
};

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const RAINBOW: &[(u8, u8, u8)] = &[
    (255, 95, 95),   // red
    (255, 165, 0),   // orange
    (240, 220, 0),   // yellow
    (95, 215, 95),   // green
    (0, 195, 255),   // cyan
    (135, 135, 255), // blue
    (215, 135, 255), // violet
];

const USAGE: &str = "gp64 [--mute] [--record FILE] [--script FILE] [--wait] [program [args]]";
const INTRO: &str = "Runs a DOS program from its own folder, or the default launcher beside gp64\nor, failing that, in the current folder.";
const OPTIONS: &[(&str, &str)] = &[
    ("--mute", "Disable host audio output"),
    (
        "--record FILE",
        "Record native video and mixed audio to MKV/WebM",
    ),
    (
        "--script FILE",
        "Run an emuscru script, then exit; implies --wait",
    ),
    (
        "--wait",
        "Windows: keep the prompt until gp64 exits, with its messages",
    ),
    ("--help", "Show this help"),
    ("--version", "Show version"),
];
const CONTROLS: &[(&str, &str)] = &[
    (
        "F12:",
        "menu (scaler, fullscreen, mute, recording, serial link, modem);",
    ),
    (
        "",
        "F12 or Escape closes. Pauses unless a link is connected. Recordings go to\nyour video folder, named by date and program; a red REC light shows\nwhile recording",
    ),
    (
        "F11:",
        "integer nearest-neighbour / smooth bilinear scaling (4:3)",
    ),
    ("Alt+Enter:", "fullscreen;"),
    ("click:", "capture mouse;"),
    ("Ctrl+F10:", "release mouse"),
];
const NOTES: &[&str] = &[
    "CPU speed adjusts automatically to host capacity; timers and audio keep normal time.",
    "Serial (COM1) and modem (COM2) links find another gp64 on the LAN by themselves.",
    "Menu choices are saved per user; command-line options win for that run.",
    "A diagnostics log, gp64.log, is kept in the per-user data folder; RUST_LOG sets its detail.",
];

/// Whether stdout is a terminal and color has not been explicitly disabled.
pub fn stdout_wants_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

fn sgr(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

fn version_banner(color: bool) -> String {
    let plain = format!("{NAME} v{VERSION}");
    if !color {
        return plain;
    }
    let mut out = String::new();
    let mut hue = 0usize;
    for ch in plain.chars() {
        if ch.is_whitespace() {
            out.push(ch);
            continue;
        }
        let (r, g, b) = RAINBOW[hue % RAINBOW.len()];
        out.push_str(&format!("\x1b[1;38;2;{r};{g};{b}m{ch}"));
        hue += 1;
    }
    out.push_str("\x1b[0m");
    out
}

/// The full help screen, with the version banner first.
pub fn help_text(color: bool) -> String {
    const OPTION_WIDTH: usize = 16;
    const CONTROL_WIDTH: usize = 12;

    let mut lines = vec![version_banner(color), String::new()];
    lines.push(sgr("usage:", "1;33", color));
    lines.push(format!("  {}", sgr(USAGE, "1", color)));
    for line in INTRO.lines() {
        lines.push(format!("  {}", sgr(line, "2", color)));
    }

    lines.push(String::new());
    lines.push(sgr("options:", "1;33", color));
    for (option, description) in OPTIONS {
        lines.push(format!(
            "  {}{} {}",
            sgr(option, "36", color),
            " ".repeat(OPTION_WIDTH.saturating_sub(option.len())),
            sgr(description, "2", color)
        ));
    }

    lines.push(String::new());
    for (control, description) in CONTROLS {
        let label = if control.is_empty() {
            String::new()
        } else {
            sgr(control, "1;33", color)
        };
        let padding = " ".repeat(CONTROL_WIDTH.saturating_sub(control.len()));
        let mut description_lines = description.lines();
        lines.push(format!(
            "  {label}{padding} {}",
            sgr(description_lines.next().unwrap_or(""), "2", color)
        ));
        for continuation in description_lines {
            lines.push(format!(
                "  {:width$} {}",
                "",
                sgr(continuation, "2", color),
                width = CONTROL_WIDTH
            ));
        }
    }
    for note in NOTES {
        lines.push(sgr(note, "2", color));
    }
    lines.join("\n")
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Help,
    Version,
    Run(Options),
}
#[derive(Debug, PartialEq)]
pub struct Options {
    pub mute: bool,
    pub record: Option<PathBuf>,
    /// An emuscru script to run against the guest.
    pub script: Option<PathBuf>,
    /// Stay attached to the console instead of detaching on Windows.
    pub wait: bool,
    pub program: Option<PathBuf>,
    pub args: String,
}
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command> {
    let mut args = args.into_iter();
    let mut options = Options {
        mute: false,
        record: None,
        script: None,
        wait: false,
        program: None,
        args: String::new(),
    };
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => return Ok(Command::Help),
            Some("--version" | "-V") => return Ok(Command::Version),
            Some("--mute") => options.mute = true,
            Some("--wait") => options.wait = true,
            Some("--record") => {
                let value = args.next().context("--record requires a value")?;
                options.record = Some(value.into());
            }
            Some("--script") => {
                let value = args.next().context("--script requires a value")?;
                options.script = Some(value.into());
            }
            Some("--") => {
                if let Some(program) = args.next() {
                    options.program = Some(program.into());
                }
                options.args = dos_args(args)?;
                break;
            }
            Some(flag) if flag.starts_with('-') => bail!("unknown option: {flag}"),
            _ => {
                options.program = Some(arg.into());
                options.args = dos_args(args)?;
                break;
            }
        }
    }
    Ok(Command::Run(options))
}
fn dos_args(args: impl IntoIterator<Item = OsString>) -> Result<String> {
    args.into_iter()
        .map(|arg| {
            let arg = arg
                .into_string()
                .map_err(|_| anyhow::anyhow!("program arguments must be text"))?;
            Ok(if arg.contains([' ', '\t']) {
                format!("\"{arg}\"")
            } else {
                arg
            })
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join(" "))
}
/// Bundled executables mount the folder beside the bundle, never its resources.
pub fn executable_directory(executable: &Path) -> Result<PathBuf> {
    let directory = executable
        .parent()
        .context("executable has no parent folder")?;
    let bundle = directory
        .ends_with("Contents/MacOS")
        .then(|| directory.parent()?.parent())
        .flatten()
        .filter(|bundle| {
            bundle
                .extension()
                .is_some_and(|extension| extension == "app")
        });
    Ok(bundle
        .and_then(Path::parent)
        .unwrap_or(directory)
        .to_path_buf())
}

/// The default launch folder: beside gp64 first, then the current folder.
pub fn default_folder(beside: &Path, current: Option<&Path>) -> Result<PathBuf> {
    let has_default = |folder: &Path| launcher::default_program(folder).is_ok();
    if has_default(beside) {
        return Ok(beside.to_path_buf());
    }
    match current {
        Some(current) if current == beside => {}
        Some(current) if has_default(current) => return Ok(current.to_path_buf()),
        Some(current) => bail!(
            "{} ({}) or in the current folder ({})",
            launcher::MISSING_DEFAULT,
            beside.display(),
            current.display()
        ),
        None => {}
    }
    bail!("{} ({})", launcher::MISSING_DEFAULT, beside.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(text: &str) -> String {
        let mut plain = String::new();
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for escape in chars.by_ref() {
                    if escape == 'm' {
                        break;
                    }
                }
            } else {
                plain.push(ch);
            }
        }
        plain
    }

    #[test]
    fn colored_help_matches_plain_and_uses_selective_styles() {
        let plain = help_text(false);
        let colored = help_text(true);

        assert_eq!(strip_ansi(&colored), plain);
        assert!(colored.contains("\x1b[1;33musage:\x1b[0m"));
        assert!(colored.contains(&format!("\x1b[1m{USAGE}\x1b[0m")));
        assert!(colored.contains("\x1b[36m--mute\x1b[0m"));
        assert!(colored.contains("\x1b[2mDisable host audio output\x1b[0m"));
        assert!(colored.contains("\x1b[1;33mF12:\x1b[0m"));
        assert!(!plain.contains('\x1b'));
    }

    #[test]
    fn help_starts_with_the_version_banner() {
        let plain = help_text(false);
        let colored = help_text(true);

        assert_eq!(plain.lines().next(), Some(version_banner(false).as_str()));
        assert_eq!(colored.lines().next(), Some(version_banner(true).as_str()));
        assert!(plain.starts_with(&format!("{NAME} v{VERSION}\n")));
    }

    #[test]
    fn version_banner_uses_multiple_rainbow_colors() {
        let banner = version_banner(true);
        let colors: std::collections::HashSet<_> = banner
            .split("\x1b[1;38;2;")
            .skip(1)
            .filter_map(|part| part.split('m').next())
            .collect();

        assert!(
            colors.len() >= 5,
            "expected a rainbow palette, got {colors:?}"
        );
        assert_eq!(strip_ansi(&banner), version_banner(false));
    }

    #[test]
    fn plain_help_documents_every_option_and_existing_guidance() {
        let text = help_text(false);
        for expected in [
            USAGE,
            "Runs a DOS program from its own folder, or the default launcher beside gp64",
            "or, failing that, in the current folder.",
            "--mute",
            "Disable host audio output",
            "--record FILE",
            "Record native video and mixed audio to MKV/WebM",
            "--script FILE",
            "Run an emuscru script, then exit; implies --wait",
            "--wait",
            "Windows: keep the prompt until gp64 exits, with its messages",
            "--help",
            "Show this help",
            "--version",
            "Show version",
            "F12:",
            "menu (scaler, fullscreen, mute, recording, serial link, modem);",
            "F12 or Escape closes. Pauses unless a link is connected. Recordings go to",
            "your video folder, named by date and program; a red REC light shows",
            "while recording",
            "F11:",
            "integer nearest-neighbour / smooth bilinear scaling (4:3)",
            "Alt+Enter:",
            "fullscreen;",
            "click:",
            "capture mouse;",
            "Ctrl+F10:",
            "release mouse",
            "CPU speed adjusts automatically to host capacity; timers and audio keep normal time.",
            "Serial (COM1) and modem (COM2) links find another gp64 on the LAN by themselves.",
            "Menu choices are saved per user; command-line options win for that run.",
            "A diagnostics log, gp64.log, is kept in the per-user data folder; RUST_LOG sets its detail.",
        ] {
            assert!(text.contains(expected), "help is missing {expected:?}");
        }
        assert!(!text.contains('\x1b'));
    }

    fn command(args: &[&str]) -> Result<Command> {
        parse(args.iter().map(OsString::from))
    }
    #[test]
    fn host_options_and_program_arguments() {
        assert_eq!(
            command(&["--mute", "dir/test.com", "a b", "--speed"]).unwrap(),
            Command::Run(Options {
                mute: true,
                record: None,
                script: None,
                wait: false,
                program: Some("dir/test.com".into()),
                args: "\"a b\" --speed".into(),
            })
        );
    }
    #[test]
    fn invalid_and_deferred_options() {
        for value in ["20", "0", "-2", "NaN", "inf", "1e30", "bad"] {
            assert!(command(&["--speed", value]).is_err());
        }
        let Command::Run(options) = command(&["--record", "out.mkv", "--mute"]).unwrap() else {
            panic!()
        };
        assert_eq!(options.record, Some("out.mkv".into()));
        assert!(options.mute && !options.wait);
        let Command::Run(options) = command(&["--wait", "game.exe", "--wait"]).unwrap() else {
            panic!()
        };
        assert!(options.wait);
        assert_eq!(options.args, "--wait");
        assert!(command(&["--record"]).is_err());
        let Command::Run(options) =
            command(&["--script", "a.emu", "game.exe", "--script"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(options.script, Some("a.emu".into()));
        assert_eq!(options.args, "--script");
        assert!(command(&["--script"]).is_err());
        for args in [
            vec!["--speed"],
            vec!["--wat"],
            // Links are set from the menu and find their peer by themselves.
            vec!["--host", "1234"],
            vec!["--join", "host:1234"],
            vec!["--com", "1"],
        ] {
            assert!(command(&args).is_err());
        }
        assert_eq!(command(&["--help"]).unwrap(), Command::Help);
        assert_eq!(command(&["--version"]).unwrap(), Command::Version);
    }
    #[test]
    fn executable_folders() {
        assert_eq!(
            executable_directory(Path::new("/a/gp64")).unwrap(),
            Path::new("/a")
        );
        assert_eq!(
            executable_directory(Path::new("/a/gp64.app/Contents/MacOS/gp64")).unwrap(),
            Path::new("/a")
        );
        assert_eq!(
            executable_directory(Path::new("/a/plain/Contents/MacOS/gp64")).unwrap(),
            Path::new("/a/plain/Contents/MacOS")
        );
    }
    #[test]
    fn default_folder_prefers_beside_gp64_then_the_current_folder() {
        let root = std::env::temp_dir().join(format!("gp64-default-folder-{}", std::process::id()));
        let (beside, current, empty) = (
            root.join("beside"),
            root.join("current"),
            root.join("empty"),
        );
        for folder in [&beside, &current, &empty] {
            std::fs::create_dir_all(folder).unwrap();
        }
        std::fs::write(beside.join(launcher::DEFAULT_PROGRAM), b"").unwrap();
        std::fs::write(current.join(launcher::DEFAULT_BATCH), b"").unwrap();
        assert_eq!(default_folder(&beside, Some(&current)).unwrap(), beside);
        assert_eq!(default_folder(&empty, Some(&current)).unwrap(), current);
        assert_eq!(
            default_folder(&empty, None).unwrap_err().to_string(),
            format!("{} ({})", launcher::MISSING_DEFAULT, empty.display())
        );
        assert_eq!(
            default_folder(&empty, Some(&empty))
                .unwrap_err()
                .to_string(),
            format!("{} ({})", launcher::MISSING_DEFAULT, empty.display())
        );
        assert_eq!(
            default_folder(&empty, Some(&root)).unwrap_err().to_string(),
            format!(
                "{} ({}) or in the current folder ({})",
                launcher::MISSING_DEFAULT,
                empty.display(),
                root.display()
            )
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
