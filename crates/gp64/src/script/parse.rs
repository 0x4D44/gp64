//! The emuscru script language, trimmed to the verbs gp64 runs.
//!
//! One command per line. `#` starts a comment at the start of a line or after
//! whitespace, outside quotes. Verbs and key names ignore case. Strings are
//! double-quoted with `\n`, `\r`, `\t`, `\\` and `\xNN` escapes; any other
//! escape keeps its backslash. `label: name` marks a target for a forward
//! `skip-to name`.
use super::keys::{self, KeyDef};
use std::{collections::HashMap, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
impl MouseButton {
    /// The INT 33h button number.
    pub fn number(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Middle => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `launch`, `status`, `speed`, `perf-begin` and `perf-end`: accepted for
    /// compatibility and logged, but gp64's own command line decides these.
    Ignored(String),
    Key {
        name: String,
        key: KeyDef,
    },
    Type(String),
    MouseClick {
        x: i32,
        y: i32,
        button: MouseButton,
    },
    MouseDoubleClick {
        x: i32,
        y: i32,
        button: MouseButton,
    },
    MouseDown {
        x: i32,
        y: i32,
        button: MouseButton,
    },
    MouseUp(MouseButton),
    Wait(u32),
    WaitStable(Option<u32>),
    Screenshot(Option<String>),
    FrameHash,
    Log(String),
    Label(String),
    SkipTo(String),
}
impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let button = |button: &MouseButton| format!("{button:?}").to_ascii_lowercase();
        match self {
            Self::Ignored(text) => write!(f, "{text}"),
            Self::Key { name, .. } => write!(f, "key {name}"),
            Self::Type(text) => write!(f, "type {text:?}"),
            Self::MouseClick { x, y, button: b } => {
                write!(f, "mouse-click {x} {y} {}", button(b))
            }
            Self::MouseDoubleClick { x, y, button: b } => {
                write!(f, "mouse-double-click {x} {y} {}", button(b))
            }
            Self::MouseDown { x, y, button: b } => write!(f, "mouse-down {x} {y} {}", button(b)),
            Self::MouseUp(b) => write!(f, "mouse-up {}", button(b)),
            Self::Wait(frames) => write!(f, "wait {frames}"),
            Self::WaitStable(Some(timeout)) => write!(f, "wait-stable {timeout}"),
            Self::WaitStable(None) => write!(f, "wait-stable"),
            Self::Screenshot(Some(path)) => write!(f, "screenshot {path}"),
            Self::Screenshot(None) => write!(f, "screenshot"),
            Self::FrameHash => write!(f, "framehash"),
            Self::Log(message) => write!(f, "log {message:?}"),
            Self::Label(name) => write!(f, "label: {name}"),
            Self::SkipTo(label) => write!(f, "skip-to {label}"),
        }
    }
}

#[derive(Debug)]
pub struct Script {
    /// Commands with their 1-based line numbers.
    pub commands: Vec<(usize, Command)>,
    /// Label name to index in `commands`.
    pub labels: HashMap<String, usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for ParseError {}

pub fn parse(input: &str) -> Result<Script, ParseError> {
    let mut commands = Vec::new();
    let mut labels = HashMap::new();
    for (index, raw) in input.lines().enumerate() {
        let line = index + 1;
        let text = strip_comment(raw).trim();
        if text.is_empty() {
            continue;
        }
        let command = parse_command(text, line)?;
        if let Command::Label(name) = &command {
            if labels.insert(name.clone(), commands.len()).is_some() {
                return Err(error(line, format!("duplicate label '{name}'")));
            }
        }
        commands.push((line, command));
    }
    for (index, (line, command)) in commands.iter().enumerate() {
        if let Command::SkipTo(label) = command {
            match labels.get(label) {
                None => {
                    return Err(error(
                        *line,
                        format!("undefined label '{label}' in skip-to"),
                    ));
                }
                Some(&target) if target <= index => {
                    return Err(error(
                        *line,
                        format!("skip-to label '{label}' must reference a forward label"),
                    ));
                }
                Some(_) => {}
            }
        }
    }
    Ok(Script { commands, labels })
}

fn error(line: usize, message: String) -> ParseError {
    ParseError { line, message }
}

/// A `#` at the start or after whitespace, outside quotes, starts a comment.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quoted = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if quoted => i += 1,
            b'"' => quoted = !quoted,
            b'#' if !quoted && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
}

fn parse_command(line: &str, number: usize) -> Result<Command, ParseError> {
    if line.len() >= 6 && line[..6].eq_ignore_ascii_case("label:") {
        let name = line[6..].trim();
        if name.is_empty() {
            return Err(error(number, "label name is empty".into()));
        }
        if !is_identifier(name) {
            return Err(error(
                number,
                format!("invalid label name '{name}' (must be [a-zA-Z_][a-zA-Z0-9_]*)"),
            ));
        }
        return Ok(Command::Label(name.into()));
    }
    let (verb, args) = split_first_word(line);
    let err = |message: String| error(number, message);
    Ok(match verb.to_ascii_lowercase().as_str() {
        "launch" => {
            if args.is_empty() {
                return Err(err("launch requires an executable path".into()));
            }
            Command::Ignored(format!("launch {args}"))
        }
        "status" => Command::Ignored("status".into()),
        "speed" => {
            let policy = single_arg(args, "speed", number)?;
            if !matches!(
                policy.to_ascii_lowercase().as_str(),
                "fixed" | "auto" | "turbo"
            ) {
                return Err(err(format!(
                    "unknown speed policy '{policy}' (expected fixed, auto, or turbo)"
                )));
            }
            Command::Ignored(format!("speed {policy}"))
        }
        "perf-begin" | "perf-end" => {
            let name = single_arg(args, verb, number)?;
            Command::Ignored(format!("{} {name}", verb.to_ascii_lowercase()))
        }
        "key" => {
            let name = single_arg(args, "key", number)?;
            let key =
                keys::lookup(&name).ok_or_else(|| err(format!("unknown key name '{name}'")))?;
            Command::Key { name, key }
        }
        "type" => Command::Type(quoted_string(args, number)?),
        "mouse-click" => {
            let (x, y, button) = xy_button(args, "mouse-click", number)?;
            Command::MouseClick { x, y, button }
        }
        "mouse-double-click" => {
            let (x, y, button) = xy_button(args, "mouse-double-click", number)?;
            Command::MouseDoubleClick { x, y, button }
        }
        "mouse-down" => {
            let (x, y, button) = xy_button(args, "mouse-down", number)?;
            Command::MouseDown { x, y, button }
        }
        "mouse-up" => Command::MouseUp(if args.trim().is_empty() {
            MouseButton::Left
        } else {
            button(args.trim(), number)?
        }),
        "wait" => {
            if args.trim().is_empty() {
                return Err(err("wait requires a numeric argument".into()));
            }
            Command::Wait(number_arg(args, "wait", number)?)
        }
        "wait-stable" => Command::WaitStable(if args.trim().is_empty() {
            None
        } else {
            Some(number_arg(args, "wait-stable", number)?)
        }),
        "screenshot" => Command::Screenshot(if args.trim().is_empty() {
            None
        } else {
            Some(single_arg(args, "screenshot", number)?)
        }),
        "framehash" | "displayhash" => Command::FrameHash,
        "log" => Command::Log(quoted_string(args, number)?),
        "skip-to" => {
            let label = single_arg(args, "skip-to", number)?;
            if !is_identifier(&label) {
                return Err(err(format!("invalid label name '{label}' in skip-to")));
            }
            Command::SkipTo(label)
        }
        _ => return Err(err(format!("unknown command '{verb}'"))),
    })
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.find(|c: char| c.is_ascii_whitespace()) {
        Some(at) => (&s[..at], s[at..].trim_start()),
        None => (s, ""),
    }
}

/// Whitespace-separated words; double quotes group, and a backslash inside
/// quotes escapes the next character.
fn words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut escaped) = (false, false);
    for ch in s.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch.is_ascii_whitespace() && !quoted {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// The first word; anything after it is ignored, as in emuscru.
fn single_arg(args: &str, verb: &str, line: usize) -> Result<String, ParseError> {
    let args = args.trim();
    if args.is_empty() {
        return Err(error(line, format!("{verb} requires an argument")));
    }
    Ok(split_first_word(args).0.into())
}

fn number_arg(args: &str, verb: &str, line: usize) -> Result<u32, ParseError> {
    let word = split_first_word(args.trim()).0;
    word.parse()
        .map_err(|e| error(line, format!("invalid number '{word}' for {verb}: {e}")))
}

fn integer(s: &str, name: &str, line: usize) -> Result<i32, ParseError> {
    s.parse()
        .map_err(|e| error(line, format!("invalid integer '{s}' for {name}: {e}")))
}

fn xy_button(args: &str, verb: &str, line: usize) -> Result<(i32, i32, MouseButton), ParseError> {
    let parts = words(args);
    if parts.len() < 2 {
        return Err(error(
            line,
            format!("{verb} requires x y [button] arguments"),
        ));
    }
    let button = match parts.get(2) {
        Some(name) => button(name, line)?,
        None => MouseButton::Left,
    };
    Ok((
        integer(&parts[0], "x", line)?,
        integer(&parts[1], "y", line)?,
        button,
    ))
}

fn button(s: &str, line: usize) -> Result<MouseButton, ParseError> {
    match s.to_ascii_lowercase().as_str() {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        _ => Err(error(
            line,
            format!("unknown mouse button '{s}' (expected left, right, or middle)"),
        )),
    }
}

/// A double-quoted string with escapes; text after the closing quote is
/// ignored, as in emuscru.
fn quoted_string(args: &str, line: usize) -> Result<String, ParseError> {
    let args = args.trim();
    if !args.starts_with('"') {
        return Err(error(
            line,
            format!("expected a quoted string, got: {args}"),
        ));
    }
    let bytes = args.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => {
                let parsed = gp64_types::util::parse_escaped_bytes(&args[1..i]);
                return String::from_utf8(parsed)
                    .map_err(|e| error(line, format!("invalid UTF-8 in string: {e}")));
            }
            _ => i += 1,
        }
    }
    Err(error(
        line,
        "unterminated string (missing closing quote)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> Command {
        let script = parse(line).unwrap();
        assert_eq!(script.commands.len(), 1);
        script.commands[0].1.clone()
    }
    fn fails(text: &str) -> ParseError {
        parse(text).unwrap_err()
    }

    #[test]
    fn comments_blank_lines_and_line_numbers() {
        let script = parse("# heading\n\n  wait 5 # five\n\tframehash\r\n").unwrap();
        assert_eq!(
            script.commands,
            vec![(3, Command::Wait(5)), (4, Command::FrameHash)]
        );
        // A `#` glued to a word is not a comment.
        assert_eq!(
            fails("wait#6").to_string(),
            "line 1: unknown command 'wait#6'"
        );
    }

    #[test]
    fn every_verb_parses() {
        assert_eq!(
            one("launch ../x/GAME.EXE --speed pentium"),
            Command::Ignored("launch ../x/GAME.EXE --speed pentium".into())
        );
        assert_eq!(one("STATUS"), Command::Ignored("status".into()));
        assert_eq!(one("speed Turbo"), Command::Ignored("speed Turbo".into()));
        assert_eq!(
            one("perf-begin run"),
            Command::Ignored("perf-begin run".into())
        );
        assert_eq!(one("perf-end run"), Command::Ignored("perf-end run".into()));
        assert!(matches!(
            one("Key down"),
            Command::Key { name, key } if name == "down" && key.scancode == 0x50 && key.extended
        ));
        assert_eq!(one("type \"123\""), Command::Type("123".into()));
        assert_eq!(
            one("mouse-click 160 148"),
            Command::MouseClick {
                x: 160,
                y: 148,
                button: MouseButton::Left
            }
        );
        assert_eq!(
            one("mouse-click 275 193 right"),
            Command::MouseClick {
                x: 275,
                y: 193,
                button: MouseButton::Right
            }
        );
        assert_eq!(
            one("mouse-double-click 160 55"),
            Command::MouseDoubleClick {
                x: 160,
                y: 55,
                button: MouseButton::Left
            }
        );
        assert_eq!(
            one("mouse-down 65 193 MIDDLE"),
            Command::MouseDown {
                x: 65,
                y: 193,
                button: MouseButton::Middle
            }
        );
        assert_eq!(one("mouse-up"), Command::MouseUp(MouseButton::Left));
        assert_eq!(one("mouse-up right"), Command::MouseUp(MouseButton::Right));
        assert_eq!(one("wait 120"), Command::Wait(120));
        assert_eq!(one("wait-stable"), Command::WaitStable(None));
        assert_eq!(one("wait-stable 600"), Command::WaitStable(Some(600)));
        assert_eq!(
            one("screenshot out/a_{step}.png"),
            Command::Screenshot(Some("out/a_{step}.png".into()))
        );
        assert_eq!(one("screenshot"), Command::Screenshot(None));
        assert_eq!(one("framehash"), Command::FrameHash);
        assert_eq!(one("displayhash"), Command::FrameHash);
        assert_eq!(one("log \"DIAL: ok\""), Command::Log("DIAL: ok".into()));
    }

    #[test]
    fn quoting_and_escapes() {
        assert_eq!(one(r#"type "a\tb\nc""#), Command::Type("a\tb\nc".into()));
        assert_eq!(one(r#"type "\x41\\""#), Command::Type("A\\".into()));
        assert_eq!(one(r#"type "C:\GAMES""#), Command::Type(r"C:\GAMES".into()));
        // `#` inside quotes is text; after the string it is a comment.
        assert_eq!(one(r#"log "a # b" # note"#), Command::Log("a # b".into()));
        assert_eq!(
            one(r#"log "say \"hi\"""#),
            Command::Log(r#"say \"hi\""#.into())
        );
    }

    #[test]
    fn labels_resolve_forward_only() {
        let script = parse("skip-to end\nwait 1\nlabel: end\nframehash").unwrap();
        assert_eq!(script.labels["end"], 2);
        assert_eq!(script.commands[2], (3, Command::Label("end".into())));
        assert_eq!(
            fails("label: top\nwait 1\nskip-to top").to_string(),
            "line 3: skip-to label 'top' must reference a forward label"
        );
        assert_eq!(
            fails("skip-to nowhere").to_string(),
            "line 1: undefined label 'nowhere' in skip-to"
        );
        assert_eq!(
            fails("label: a\nlabel: a").to_string(),
            "line 2: duplicate label 'a'"
        );
        assert_eq!(
            fails("label: 9x").to_string(),
            "line 1: invalid label name '9x' (must be [a-zA-Z_][a-zA-Z0-9_]*)"
        );
    }

    #[test]
    fn errors_name_the_line() {
        let cases = [
            (
                "wait 1\nwait-hash 0x1234",
                "line 2: unknown command 'wait-hash'",
            ),
            ("\n\nfrobnicate", "line 3: unknown command 'frobnicate'"),
            ("key Hyper", "line 1: unknown key name 'Hyper'"),
            ("key", "line 1: key requires an argument"),
            ("wait", "line 1: wait requires a numeric argument"),
            (
                "wait x",
                "line 1: invalid number 'x' for wait: invalid digit found in string",
            ),
            (
                "mouse-click 5",
                "line 1: mouse-click requires x y [button] arguments",
            ),
            (
                "mouse-click 5 6 thumb",
                "line 1: unknown mouse button 'thumb' (expected left, right, or middle)",
            ),
            ("type abc", "line 1: expected a quoted string, got: abc"),
            (
                "log \"open",
                "line 1: unterminated string (missing closing quote)",
            ),
            ("launch", "line 1: launch requires an executable path"),
            (
                "speed warp",
                "line 1: unknown speed policy 'warp' (expected fixed, auto, or turbo)",
            ),
        ];
        for (text, message) in cases {
            assert_eq!(fails(text).to_string(), message, "{text}");
        }
    }

    /// A script in the shape of the program's existing test scripts.
    const SAMPLE: &str = "\
# Navigate the setup menus
launch ../corpus/GAME/GAME.EXE --modem 14400 --speed pentium

wait-stable 600
key Escape
wait 120
mouse-click 160 148
wait 120
key Down
wait 10
key Enter
mouse-click 235 70
type \"123\"
screenshot tmp/emuscru/menu_00.png
framehash
status
speed turbo
log \"DIAL: Connecting...\"
mouse-double-click 275 193
mouse-down 65 193
mouse-up
perf-begin p
wait 300
perf-end p
";

    #[test]
    fn a_representative_script_parses() {
        let script = parse(SAMPLE).unwrap();
        assert_eq!(script.commands.len(), 22);
        assert_eq!(script.commands[1], (4, Command::WaitStable(Some(600))));
    }

    /// Parses every `*.emu` in `GP64_SCRIPT_DIR`, when set.
    #[test]
    fn scripts_in_gp64_script_dir_parse() {
        let Some(dir) = std::env::var_os("GP64_SCRIPT_DIR") else {
            return;
        };
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "emu") {
                let text = std::fs::read_to_string(&path).unwrap();
                if let Err(error) = parse(&text) {
                    panic!("{}: {error}", path.display());
                }
                count += 1;
            }
        }
        eprintln!("parsed {count} scripts");
        assert!(count > 0, "GP64_SCRIPT_DIR holds no .emu scripts");
    }
}
