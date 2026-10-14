//! DOS Batch File Interpreter

/// Decode raw DOS text bytes (CP437 or any other OEM code page) into a
/// `String` byte-preservingly: each byte `0x00..=0xFF` maps to the Unicode
/// code point of the same value (latin-1 identity). This never fails.
///
/// DOS text is never UTF-8 — 1990s batch files routinely carry high-bit
/// CP437 box-drawing bytes for ASCII-art banners, and validating them as a
/// UTF-8 stream rejects perfectly valid files (MDD-BUG-KILN-00636). The
/// mapping is deliberately *not* a CP437→Unicode glyph translation: the
/// guest consumes bytes, so the only requirement is that
/// [`encode_dos_text`] recovers the original byte stream exactly, which the
/// identity mapping guarantees.
pub fn decode_dos_text(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Re-encode a `String` produced by [`decode_dos_text`] back into DOS
/// bytes: chars `U+0000..=U+00FF` map back to their byte; anything outside
/// that range (which cannot have come from `decode_dos_text`) becomes `?`.
pub fn encode_dos_text(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

/// Largest batch file the interpreter will load, top-level or via `CALL`.
pub const MAX_BATCH_FILE_BYTES: u64 = 1 << 20;
/// Deepest chain of `CALL`ed batch files retained at once.
pub const MAX_BATCH_CALL_DEPTH: usize = 32;
/// Host bytes all retained batch frames (active plus `CALL` parents) may hold.
pub const MAX_BATCH_RETAINED_BYTES: usize = 16 << 20;

/// Read batch text through a capped stream: at most
/// [`MAX_BATCH_FILE_BYTES`] are accepted, and one byte more is an
/// `InvalidData` error rather than an unbounded allocation.
pub fn read_batch_text(reader: impl std::io::Read) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    reader
        .take(MAX_BATCH_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BATCH_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("batch file exceeds {MAX_BATCH_FILE_BYTES} bytes"),
        ));
    }
    Ok(decode_dos_text(&bytes))
}

/// Read a host-side batch file as DOS text. Reads raw bytes and decodes
/// them with [`decode_dos_text`], so a `.BAT` full of CP437 art loads the
/// same as an ASCII one. Besides real I/O failure, a file larger than
/// [`MAX_BATCH_FILE_BYTES`] is rejected.
pub fn read_batch_file(path: &std::path::Path) -> std::io::Result<String> {
    read_batch_text(std::fs::File::open(path)?)
}

/// Wait state for non-blocking CHOICE/PAUSE in the batch interpreter.
///
/// The batch interpreter runs inside `step()` on the main thread. Blocking on
/// stdin freezes the GUI. Instead, CHOICE and PAUSE set a wait state and return
/// immediately. On each subsequent step cycle the emulator checks for keyboard
/// input and clears the wait when a valid key arrives.
#[derive(Debug, Clone)]
pub enum BatchWait {
    /// No wait — continue processing batch lines normally.
    None,
    /// Waiting for one of the `allowed_keys` (uppercased).
    Choice {
        allowed_keys: String,
        no_prompt: bool,
    },
    /// Waiting for any key press.
    Pause,
}

/// How a DOS command interpreter-style redirection opens its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectMode {
    /// `< file`: open the existing file for reading.
    Input,
    /// `> file`: create the file, truncating any existing one.
    Truncate,
    /// `>> file`: open (or create) the file and position at its end.
    Append,
}

/// A standard handle's binding saved while a redirection replaces it, so
/// [`crate::Dos::restore_std_handle`] can put the shell's own handle back.
#[derive(Debug, Clone)]
pub struct StdHandleRedirect {
    pub(crate) std_handle: u16,
    pub(crate) saved: SavedStdBinding,
}

/// What a standard handle pointed at before it was redirected.
#[derive(Debug, Clone)]
pub(crate) enum SavedStdBinding {
    /// It referred to an open file; this private duplicate keeps that file
    /// open until the restore hands it back.
    Dup(u16),
    /// It was the default device, or an alias of another handle.
    Alias(Option<u16>),
}

#[derive(Debug, Clone)]
pub struct BatchState {
    /// Lines of the batch file
    pub lines: Vec<String>,
    /// Current line being executed (0-indexed)
    pub current_line: usize,
    /// Last program exit code (for IF ERRORLEVEL)
    pub last_exit_code: u8,
    /// ECHO state (default ON)
    pub echo_on: bool,
    /// Arguments (%0 to %9)
    pub args: Vec<String>,
    /// Output redirection: (filename, append_mode)
    pub redirect_output: Option<(String, bool)>,
    /// Input redirection (`< file`) for the current line.
    pub redirect_input: Option<String>,
    /// Standard-handle redirections in force for the child program the
    /// current line EXECed; undone, in reverse, when that child terminates.
    pub child_redirects: Vec<StdHandleRedirect>,
    /// Current wait state (CHOICE / PAUSE).
    pub wait: BatchWait,
}

impl BatchState {
    pub fn new(content: &str, args: &[String]) -> Self {
        // Trim ASCII whitespace only: batch text is decoded byte-per-char
        // (see `decode_dos_text`), and Unicode-aware `str::trim` would eat
        // CP437 bytes 0x85/0xA0 (mapped to U+0085/U+00A0) at line edges.
        let lines = content
            .lines()
            .map(|l| {
                l.trim_matches(|c: char| c.is_ascii_whitespace())
                    .to_string()
            })
            .filter(|l| !l.is_empty())
            .collect();

        Self {
            lines,
            current_line: 0,
            last_exit_code: 0,
            echo_on: true,
            args: args.to_vec(),
            redirect_output: None,
            redirect_input: None,
            child_redirects: Vec::new(),
            wait: BatchWait::None,
        }
    }

    /// Host bytes this frame retains for its lines and arguments, counting
    /// each owned string's header so many tiny lines are not free.
    pub fn retained_bytes(&self) -> usize {
        let header = std::mem::size_of::<String>();
        self.lines
            .iter()
            .chain(self.args.iter())
            .map(|text| text.len() + header)
            .sum()
    }

    pub fn is_finished(&self) -> bool {
        self.current_line >= self.lines.len()
    }

    pub fn next_line(&mut self) -> Option<String> {
        if self.is_finished() {
            None
        } else {
            let line = self.lines[self.current_line].clone();
            self.current_line += 1;
            Some(line)
        }
    }

    /// Jump to a label (starting with :)
    pub fn goto(&mut self, label: &str) -> bool {
        let target = label.trim_start_matches(':').to_uppercase();
        for (i, line) in self.lines.iter().enumerate() {
            if line.starts_with(':') {
                let l = line.trim_start_matches(':').trim().to_uppercase();
                if l == target {
                    self.current_line = i + 1;
                    return true;
                }
            }
        }
        false
    }

    /// Expand variables like %1 or %VAR%
    pub fn expand_variables(
        &self,
        line: &str,
        env: &std::collections::HashMap<String, String>,
    ) -> String {
        let mut result = line.to_string();

        // 1. Expand positional arguments %0-%9
        for i in 0..10 {
            let placeholder = format!("%{i}");
            if result.contains(&placeholder) {
                let val = self.args.get(i).cloned().unwrap_or_default();
                result = result.replace(&placeholder, &val);
            }
        }

        // 2. Expand environment variables %VAR%
        // Simple parser for %tokens%
        let mut final_res = String::new();
        let mut chars = result.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                let mut var_name = String::new();
                let mut found_end = false;
                while let Some(&next_c) = chars.peek() {
                    if next_c == '%' {
                        chars.next();
                        found_end = true;
                        break;
                    }
                    var_name.push(chars.next().unwrap());
                }
                if found_end {
                    if var_name.is_empty() {
                        final_res.push('%');
                    } else if let Some(val) = env.get(&var_name.to_uppercase()) {
                        final_res.push_str(val);
                    }
                } else {
                    final_res.push('%');
                    final_res.push_str(&var_name);
                }
            } else {
                final_res.push(c);
            }
        }

        final_res
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    // The fixture is deliberately invalid UTF-8 — that IS the premise.
    #[allow(invalid_from_utf8)]
    fn test_decode_dos_text_accepts_invalid_utf8_and_round_trips() {
        // CP437 double-line box corner bytes — invalid as UTF-8.
        let bytes: &[u8] = b"\xC9\xCD\xCD\xBB banner \xC8\xBC";
        assert!(std::str::from_utf8(bytes).is_err(), "premise: not UTF-8");

        let text = decode_dos_text(bytes);
        assert_eq!(text.chars().count(), bytes.len());
        assert_eq!(encode_dos_text(&text), bytes, "byte round-trip");
    }

    #[test]
    fn test_encode_dos_text_maps_non_latin1_chars_to_question_mark() {
        assert_eq!(encode_dos_text("a\u{2554}b"), b"a?b");
    }

    #[test]
    fn read_batch_text_accepts_the_cap_and_rejects_one_byte_more() {
        let cap = MAX_BATCH_FILE_BYTES as usize;
        let at_cap = read_batch_text(std::io::Cursor::new(vec![b'A'; cap])).expect("at cap");
        assert_eq!(at_cap.len(), cap);
        let error = read_batch_text(std::io::Cursor::new(vec![b'A'; cap + 1]))
            .expect_err("cap plus one must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn test_read_batch_file_loads_cp437_bat() {
        // Regression for MDD-BUG-KILN-00636: a .BAT containing CP437
        // box-drawing bytes must load (the old reader used
        // `read_to_string` and failed the whole launch with
        // "stream did not contain valid UTF-8").
        let dir = std::env::temp_dir();
        let path = dir.join(format!("gp64_kiln00636_{}.bat", std::process::id()));
        std::fs::write(&path, b"@echo off\r\necho \xC9\xCD\xBB\r\n").unwrap();

        let content = read_batch_file(&path).expect("CP437 batch file must load");
        std::fs::remove_file(&path).ok();

        let state = BatchState::new(&content, &[]);
        assert_eq!(state.lines.len(), 2);
        assert_eq!(
            encode_dos_text(&state.lines[1]),
            b"echo \xC9\xCD\xBB",
            "high-bit bytes survive into the batch line"
        );
    }

    #[test]
    fn test_batch_lines_keep_cp437_0x85_and_0xa0_at_line_edges() {
        // MDD-BUG-ANVIL-00099: pins the ASCII-only trim in `BatchState::new`.
        // Decoded CP437 0x85 (ù, decoded as U+0085 NEL) and 0xA0 (á, decoded
        // as U+00A0 NBSP) sit at line EDGES; a Unicode-aware `str::trim()`
        // treats both as whitespace and silently eats them
        // (MDD-BUG-KILN-00636's declared second defence).
        let text = decode_dos_text(b"\xA0lead\r\ntrail\x85\r\n  spaced  \r\n");
        let state = BatchState::new(&text, &[]);
        assert_eq!(state.lines.len(), 3);
        assert_eq!(encode_dos_text(&state.lines[0]), b"\xA0lead");
        assert_eq!(encode_dos_text(&state.lines[1]), b"trail\x85");
        assert_eq!(state.lines[2], "spaced", "ASCII whitespace still trims");
    }

    #[test]
    fn test_batch_state_new_trims_and_filters() {
        let args = vec!["batch.bat".to_string(), "arg1".to_string()];
        let state = BatchState::new("  echo hi  \n\n:Label \n  \n", &args);

        assert_eq!(state.lines, vec!["echo hi", ":Label"]);
        assert_eq!(state.args, args);
        assert!(state.echo_on);
    }

    #[test]
    fn test_next_line_and_finished() {
        let mut state = BatchState::new("line1\nline2", &[]);

        assert!(!state.is_finished());
        assert_eq!(state.next_line().as_deref(), Some("line1"));
        assert_eq!(state.next_line().as_deref(), Some("line2"));
        assert!(state.is_finished());
        assert!(state.next_line().is_none());
    }

    #[test]
    fn test_goto_finds_label_and_ignores_case() {
        let mut state = BatchState::new("line1\n:Foo\nline2\n:BAR\nline3", &[]);

        assert!(state.goto("bar"));
        assert_eq!(state.next_line().as_deref(), Some("line3"));
        assert!(!state.goto(":missing"));
    }

    #[test]
    fn test_expand_variables_positional_and_env() {
        let args = vec![
            "batch.bat".to_string(),
            "one".to_string(),
            "two".to_string(),
        ];
        let state = BatchState::new("echo %0 %1 %2 %FOO% %missing%", &args);
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());

        let expanded = state.expand_variables("echo %0 %1 %2 %FOO% %missing%", &env);
        assert_eq!(expanded, "echo batch.bat one two bar ");
    }

    #[test]
    fn test_expand_variables_preserves_incomplete_marker() {
        let state = BatchState::new("echo %", &[]);
        let env = HashMap::new();

        let expanded = state.expand_variables("value %", &env);
        assert_eq!(expanded, "value %");
    }

    #[test]
    fn test_goto_label_with_spaces() {
        let mut state = BatchState::new("echo one\n:My Label\n echo two", &[]);

        assert!(state.goto("my label"));
        assert_eq!(state.next_line().as_deref(), Some("echo two"));
    }

    #[test]
    fn test_expand_variables_env_case_insensitive() {
        let state = BatchState::new("echo %foo%", &[]);
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());

        let expanded = state.expand_variables("echo %foo%", &env);
        assert_eq!(expanded, "echo bar");
    }

    #[test]
    fn test_expand_variables_multiple_env_tokens() {
        let args = vec!["batch.bat".to_string(), "arg1".to_string()];
        let state = BatchState::new("set %FOO%-%BAR% %1", &args);
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "hello".to_string());
        env.insert("BAR".to_string(), "world".to_string());

        let expanded = state.expand_variables("set %FOO%-%BAR% %1", &env);
        assert_eq!(expanded, "set hello-world arg1");
    }

    #[test]
    fn test_expand_variables_missing_args_are_empty() {
        let args = vec!["batch.bat".to_string()];
        let state = BatchState::new("echo %1 %9", &args);
        let env = HashMap::new();

        let expanded = state.expand_variables("echo %1 %9", &env);
        assert_eq!(expanded, "echo  ");
    }

    #[test]
    fn test_expand_variables_empty_env_token() {
        let state = BatchState::new("echo %%", &[]);
        let env = HashMap::new();

        let expanded = state.expand_variables("echo %%", &env);
        assert_eq!(expanded, "echo %");
    }

    #[test]
    fn test_expand_variables_double_percent_in_text() {
        let state = BatchState::new("A%%B", &[]);
        let env = HashMap::new();

        let expanded = state.expand_variables("A%%B", &env);
        assert_eq!(expanded, "A%B");
    }

    #[test]
    fn test_expand_variables_missing_env_drops_token() {
        let state = BatchState::new("echo %MISSING%", &[]);
        let env = HashMap::new();

        let expanded = state.expand_variables("echo %MISSING%", &env);
        assert_eq!(expanded, "echo ");
    }

    #[test]
    fn test_expand_variables_incomplete_env_token_keeps_percent() {
        let state = BatchState::new("echo %FOO", &[]);
        let env = HashMap::new();

        let expanded = state.expand_variables("echo %FOO", &env);
        assert_eq!(expanded, "echo %FOO");
    }

    #[test]
    fn test_goto_missing_label_does_not_move() {
        let mut state = BatchState::new("line1\n:Label\nline2", &[]);

        assert!(!state.goto("missing"));
        assert_eq!(state.next_line().as_deref(), Some("line1"));
    }

    // ========== redirect_output field ==========

    #[test]
    fn test_redirect_output_default_none() {
        let state = BatchState::new("echo hi", &[]);
        assert!(state.redirect_output.is_none());
    }

    #[test]
    fn test_redirect_output_can_be_set() {
        let mut state = BatchState::new("echo hi", &[]);
        state.redirect_output = Some(("output.txt".to_string(), false));
        assert_eq!(
            state.redirect_output,
            Some(("output.txt".to_string(), false))
        );
    }

    // ========== last_exit_code and echo_on ==========

    #[test]
    fn test_last_exit_code_default() {
        let state = BatchState::new("echo", &[]);
        assert_eq!(state.last_exit_code, 0);
    }

    #[test]
    fn test_echo_toggle() {
        let mut state = BatchState::new("echo", &[]);
        assert!(state.echo_on);
        state.echo_on = false;
        assert!(!state.echo_on);
    }

    // ========== empty batch ==========

    #[test]
    fn test_empty_batch_is_finished() {
        let state = BatchState::new("", &[]);
        assert!(state.is_finished());
        assert_eq!(state.lines.len(), 0);
    }

    // ========== goto with colon prefix ==========

    #[test]
    fn test_goto_strips_leading_colon() {
        let mut state = BatchState::new("line1\n:MyLabel\nline2", &[]);
        assert!(state.goto(":MyLabel"));
        assert_eq!(state.next_line().as_deref(), Some("line2"));
    }

    // ========== Clone and Debug ==========

    #[test]
    fn test_batch_state_clone() {
        let state = BatchState::new("line1\nline2", &["arg0".to_string()]);
        let cloned = state.clone();
        assert_eq!(cloned.lines, state.lines);
        assert_eq!(cloned.args, state.args);
    }

    #[test]
    fn test_batch_state_debug() {
        let state = BatchState::new("echo hi", &[]);
        let debug_str = format!("{:?}", state);
        assert!(debug_str.contains("BatchState"));
    }

    // ========== expand_variables no percent signs ==========

    #[test]
    fn test_expand_variables_no_percent_signs() {
        let state = BatchState::new("echo hello", &[]);
        let env = HashMap::new();
        let expanded = state.expand_variables("echo hello", &env);
        assert_eq!(expanded, "echo hello");
    }
}
