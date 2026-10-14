//! Cooperative DOS batch execution. Each step consumes at most one command.
use anyhow::{anyhow, bail, ensure, Result};
use gp64_dos::shell::{decode_dos_text, encode_dos_text, BatchState, BatchWait, RedirectMode};
use std::{
    collections::{HashMap, VecDeque},
    io::Read,
};

#[derive(Debug, PartialEq, Eq)]
pub enum BatchAction {
    Continue,
    Launch { program: String, args: String },
    Finished(u8),
    Waiting,
}

pub struct BatchRunner {
    state: BatchState,
    parents: Vec<(BatchState, VecDeque<String>)>,
    pending: VecDeque<String>,
    child_running: bool,
    restore_redirects: bool,
}

#[cfg(test)]
thread_local! {
    // Observe the allocation boundary without attempting an oversized expansion.
    static FOR_MATERIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl BatchRunner {
    fn enqueue_for_commands(&mut self, var: &str, command: &str, items: &[String]) -> Result<()> {
        const MAX_PENDING: usize = 4096;
        let limit = gp64_dos::shell::MAX_BATCH_RETAINED_BYTES;
        ensure!(items.len() <= MAX_PENDING, "FOR set too large");
        ensure!(
            items
                .len()
                .checked_add(self.pending.len())
                .is_some_and(|count| count <= MAX_PENDING),
            "FOR pending commands exceed batch limit"
        );

        let header = std::mem::size_of::<String>();
        let mut bytes = 0usize;
        let mut add = |length: usize| -> Result<()> {
            bytes = bytes
                .checked_add(length)
                .and_then(|total| total.checked_add(header))
                .filter(|total| *total <= limit)
                .ok_or_else(|| anyhow!("FOR pending commands exceed batch limit"))?;
            Ok(())
        };
        // Account for the active frame and every suspended frame before
        // allocating any replacement. Retained source lines remain live.
        for (state, pending) in std::iter::once((&self.state, &self.pending))
            .chain(self.parents.iter().map(|(state, pending)| (state, pending)))
        {
            for text in state.lines.iter().chain(&state.args).chain(pending) {
                add(text.len())?;
            }
        }
        let occurrences = command.matches(var).count();
        let fixed = command
            .len()
            .checked_sub(
                occurrences
                    .checked_mul(var.len())
                    .ok_or_else(|| anyhow!("FOR pending commands exceed batch limit"))?,
            )
            .and_then(|length| length.checked_add(1))
            .ok_or_else(|| anyhow!("FOR pending commands exceed batch limit"))?;
        for item in items {
            let length = occurrences
                .checked_mul(item.len())
                .and_then(|length| length.checked_add(fixed))
                .ok_or_else(|| anyhow!("FOR pending commands exceed batch limit"))?;
            add(length)?;
        }

        let generated: Vec<_> = items
            .iter()
            .map(|item| {
                #[cfg(test)]
                FOR_MATERIALIZATIONS.with(|count| count.set(count.get() + 1));
                format!("@{}", command.replace(var, item))
            })
            .collect();
        for command in generated.into_iter().rev() {
            self.pending.push_front(command);
        }
        Ok(())
    }
    pub fn new(content: &str, program: &str, args: &str) -> Self {
        let mut argv = vec![program.to_owned()];
        argv.extend(tokens(args));
        Self {
            state: BatchState::new(content, &argv),
            parents: Vec::new(),
            pending: VecDeque::new(),
            child_running: false,
            restore_redirects: false,
        }
    }

    pub fn program_exited(&mut self, code: u8) {
        self.state.last_exit_code = code;
        self.child_running = false;
        self.restore_redirects = true;
    }

    pub fn next(
        &mut self,
        dos: &mut gp64_dos::Dos,
        machine: &mut crate::Machine,
    ) -> Result<BatchAction> {
        let result = self.next_command(dos, machine);
        if result.is_err() {
            // The caller may drop the runner on error. Release saved bindings
            // before returning, while retaining command/frame positions for
            // callers that choose to resume after a failed command.
            self.restore_all_redirects(dos);
        }
        result
    }

    pub(crate) fn restore_all_redirects(&mut self, dos: &mut gp64_dos::Dos) {
        restore(dos, &mut self.state);
        for (state, _) in self.parents.iter_mut().rev() {
            restore(dos, state);
        }
    }

    fn next_command(
        &mut self,
        dos: &mut gp64_dos::Dos,
        machine: &mut crate::Machine,
    ) -> Result<BatchAction> {
        if self.child_running {
            return Ok(BatchAction::Waiting);
        }
        if self.restore_redirects {
            restore(dos, &mut self.state);
            self.restore_redirects = false;
        }
        dos.set_last_exit_code(self.state.last_exit_code);
        if !matches!(self.state.wait, BatchWait::None) {
            let Some((_, ascii)) = gp64_hw::bda::kbd_dequeue(&mut machine.memory) else {
                return Ok(BatchAction::Waiting);
            };
            match &self.state.wait {
                BatchWait::Pause => self.state.wait = BatchWait::None,
                BatchWait::Choice {
                    allowed_keys,
                    no_prompt,
                } => {
                    let key = (ascii as char).to_ascii_uppercase();
                    if let Some(index) = allowed_keys.chars().position(|c| c == key) {
                        self.state.last_exit_code = (index + 1) as u8;
                        if !no_prompt {
                            output(dos, machine, &format!("{key}\r\n"))?;
                        }
                        self.state.wait = BatchWait::None;
                    }
                }
                BatchWait::None => {}
            }
            return Ok(BatchAction::Continue);
        }
        let Some(line) = self.pending.pop_front().or_else(|| self.state.next_line()) else {
            let code = self.state.last_exit_code;
            if let Some((parent, pending)) = self.parents.pop() {
                restore(dos, &mut self.state);
                self.state = parent;
                self.pending = pending;
                restore(dos, &mut self.state);
                self.state.last_exit_code = code;
                return Ok(BatchAction::Continue);
            }
            return Ok(BatchAction::Finished(code));
        };
        let silent = line.starts_with('@');
        let line = line.trim_start_matches('@');
        if line.starts_with(':') {
            return Ok(BatchAction::Continue);
        }
        let env = environment(dos, machine);
        // Preserve FOR markers until substitution, then expand the command's variables.
        let expanded = if line
            .get(..4)
            .is_some_and(|s| s.eq_ignore_ascii_case("for "))
        {
            line.to_owned()
        } else {
            self.state.expand_variables(line, &env)
        };
        if self.state.echo_on && !silent {
            output(dos, machine, &format!("{expanded}\r\n"))?;
        }
        let command_name = split_command(&expanded).0;
        let (command, redirects) = if command_name.eq_ignore_ascii_case("if")
            || command_name.eq_ignore_ascii_case("for")
        {
            (expanded.clone(), Vec::new())
        } else {
            parse_redirects(&expanded)?
        };
        for (handle, file, mode) in redirects {
            match dos.redirect_std_handle(handle, &file, mode) {
                Ok(saved) => self.state.child_redirects.push(saved),
                Err(code) => {
                    restore(dos, &mut self.state);
                    bail!("cannot redirect {file}: DOS error {code}");
                }
            }
        }
        let result = self.command(&command, dos, machine);
        match &result {
            Ok(BatchAction::Launch { .. }) => self.child_running = true,
            _ => restore(dos, &mut self.state),
        }
        result
    }

    fn command(
        &mut self,
        line: &str,
        dos: &mut gp64_dos::Dos,
        machine: &mut crate::Machine,
    ) -> Result<BatchAction> {
        let (name, rest) = split_command(line);
        let args = tokens(rest);
        let upper = name.to_ascii_uppercase();
        match upper.as_str() {
            "" | "REM" => {}
            "ECHO" => {
                if rest.eq_ignore_ascii_case("off") {
                    self.state.echo_on = false;
                } else if rest.eq_ignore_ascii_case("on") {
                    self.state.echo_on = true;
                } else {
                    let text = if let Some(text) = rest.strip_prefix('.') {
                        text
                    } else if rest.is_empty() {
                        if self.state.echo_on {
                            "ECHO is on"
                        } else {
                            "ECHO is off"
                        }
                    } else {
                        rest
                    };
                    output(dos, machine, &format!("{text}\r\n"))?;
                }
            }
            "SET" | "PATH" => {
                if upper == "PATH" && !rest.is_empty() {
                    set_environment(dos, machine, "PATH", rest.trim_start_matches('='))?;
                } else if let Some((key, value)) = rest.split_once('=') {
                    set_environment(dos, machine, key.trim(), value)?;
                } else {
                    let mut env: Vec<_> = environment(dos, machine).into_iter().collect();
                    env.sort();
                    for (key, value) in env {
                        if upper != "PATH" || key == "PATH" {
                            output(dos, machine, &format!("{key}={value}\r\n"))?;
                        }
                    }
                }
            }
            "CD" | "CHDIR" => {
                if rest.is_empty() {
                    let drive = dos.path_resolver.get_current_drive();
                    output(
                        dos,
                        machine,
                        &format!(
                            "{}:{}\r\n",
                            (b'A' + drive) as char,
                            dos.path_resolver.get_current_dir(drive)
                        ),
                    )?;
                } else {
                    let resolved = dos.path_resolver.resolve(rest.trim_matches('"'));
                    let attrs = drive(dos, resolved.drive)?.get_attributes(&resolved.path)?;
                    ensure!(attrs.directory, "not a directory: {rest}");
                    dos.path_resolver.set_current_dir(rest.trim_matches('"'));
                }
            }
            "MD" | "MKDIR" | "RD" | "RMDIR" | "DEL" | "ERASE" => {
                ensure!(!args.is_empty(), "{name} requires a path");
                for path in args {
                    let resolved = dos.path_resolver.resolve(&path);
                    let drive = drive(dos, resolved.drive)?;
                    match upper.as_str() {
                        "MD" | "MKDIR" => drive.create_dir(&resolved.path)?,
                        "RD" | "RMDIR" => drive.remove_dir(&resolved.path)?,
                        _ => drive.remove_file(&resolved.path)?,
                    }
                }
            }
            "REN" | "RENAME" => {
                ensure!(args.len() == 2, "REN requires source and destination");
                let from = dos.path_resolver.resolve(&args[0]);
                let to = if args[1].contains(['\\', '/', ':']) {
                    dos.path_resolver.resolve(&args[1])
                } else {
                    let parent = from.path.rsplit_once('\\').map_or("", |(p, _)| p);
                    dos.path_resolver.resolve(&format!(
                        "{}:{parent}\\{}",
                        (b'A' + from.drive) as char,
                        args[1]
                    ))
                };
                ensure!(from.drive == to.drive, "cannot rename across drives");
                drive(dos, from.drive)?.rename(&from.path, &to.path)?;
            }
            "TYPE" | "MORE" => {
                for path in args {
                    let resolved = dos.path_resolver.resolve(&path);
                    let mut file = drive(dos, resolved.drive)?.open_file(
                        &resolved.path,
                        false,
                        false,
                        false,
                    )?;
                    let mut buffer = [0; 4096];
                    loop {
                        let n = file.read(&mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        output(dos, machine, &decode_dos_text(&buffer[..n]))?;
                    }
                }
            }
            "COPY" => {
                ensure!(args.len() == 2, "COPY requires source and destination");
                let from = dos.path_resolver.resolve(&args[0]);
                let to = dos.path_resolver.resolve(&args[1]);
                ensure!(from != to, "cannot copy a file onto itself");
                let mut source =
                    drive(dos, from.drive)?.open_file(&from.path, false, false, false)?;
                // Open without truncation, then compare the underlying handles.
                // DOS path equality alone misses SUBST and host filesystem aliases.
                let mut target = drive(dos, to.drive)?.open_file(&to.path, true, true, false)?;
                copy_file_contents(source.as_mut(), target.as_mut())?;
            }
            "CLS" => {
                for cell in 0..2000 {
                    machine.memory.write_u16(0xb8000 + cell * 2, 0x0720);
                }
                machine.io.vga.set_cursor(0, 0, 0);
                machine.memory.write_u16(0x450, 0);
            }
            "PAUSE" => {
                output(dos, machine, "Press any key to continue . . .\r\n")?;
                self.state.wait = BatchWait::Pause;
            }
            "CHOICE" => {
                let mut keys = "YN".to_owned();
                let mut no_prompt = false;
                let mut prompt = Vec::new();
                for arg in args {
                    let option = arg.to_ascii_uppercase();
                    if let Some(value) = option
                        .strip_prefix("/C:")
                        .or_else(|| option.strip_prefix("/C"))
                    {
                        keys = value.to_owned();
                    } else if option == "/N" {
                        no_prompt = true;
                    } else if !option.starts_with('/') {
                        prompt.push(arg);
                    }
                }
                ensure!(
                    !keys.is_empty() && keys.len() <= 255,
                    "invalid CHOICE key list"
                );
                let prompt = prompt.join(" ");
                output(
                    dos,
                    machine,
                    &if no_prompt {
                        prompt
                    } else {
                        format!("{prompt} [{keys}]?")
                    },
                )?;
                self.state.wait = BatchWait::Choice {
                    allowed_keys: keys,
                    no_prompt,
                };
            }
            "GOTO" => {
                if rest.eq_ignore_ascii_case(":eof") {
                    self.state.current_line = self.state.lines.len();
                    self.pending.clear();
                } else {
                    ensure!(self.state.goto(rest), "label not found: {rest}");
                    self.pending.clear();
                }
            }
            "SUBST" => {
                if args.is_empty() {
                    for (index, path) in dos.subst_mappings() {
                        output(
                            dos,
                            machine,
                            &format!("{}:\\: => {}\r\n", (b'A' + index) as char, path.display()),
                        )?;
                    }
                } else {
                    ensure!(
                        args.len() == 2
                            && args[0].len() == 2
                            && args[0].ends_with(':')
                            && args[0].as_bytes()[0].is_ascii_alphabetic(),
                        "SUBST requires a drive and path or /D"
                    );
                    let index = args[0].as_bytes()[0].to_ascii_uppercase() - b'A';
                    if args[1].eq_ignore_ascii_case("/d") {
                        dos.subst_delete(index).map_err(|error| anyhow!(error))?;
                    } else {
                        let resolved = dos.path_resolver.resolve(&args[1]);
                        let host = dos
                            .drives
                            .get(&resolved.drive)
                            .and_then(|drive| drive.resolve_host_path(&resolved.path))
                            .filter(|path| path.is_dir())
                            .ok_or_else(|| anyhow!("SUBST directory not found: {}", args[1]))?;
                        dos.subst_create(index, host)
                            .map_err(|error| anyhow!(error))?;
                    }
                }
            }
            "SHIFT" => {
                if self.state.args.len() > 1 {
                    self.state.args.remove(0);
                }
            }
            "EXIT" => {
                if let Some(code) = args.iter().find_map(|arg| arg.parse::<u8>().ok()) {
                    self.state.last_exit_code = code;
                }
                self.state.current_line = self.state.lines.len();
                self.pending.clear();
                if !args.iter().any(|arg| arg.eq_ignore_ascii_case("/b")) {
                    self.restore_all_redirects(dos);
                    self.parents.clear();
                }
            }
            "CALL" => {
                let (program, tail) = split_command(rest);
                ensure!(!program.is_empty(), "CALL requires a program");
                let (program, file) = crate::dosrun::resolve_program(
                    dos,
                    &machine.memory,
                    program.trim_matches('"'),
                )?;
                if program.to_ascii_uppercase().ends_with(".BAT") {
                    ensure!(
                        self.parents.len() < gp64_dos::shell::MAX_BATCH_CALL_DEPTH,
                        "batch nesting too deep"
                    );
                    let content = gp64_dos::shell::read_batch_text(file)?;
                    let mut argv = vec![program.to_owned()];
                    argv.extend(tokens(tail));
                    let child = BatchState::new(&content, &argv);
                    let bytes = child.retained_bytes()
                        + self.state.retained_bytes()
                        + self
                            .parents
                            .iter()
                            .map(|(state, pending)| {
                                state.retained_bytes()
                                    + pending.iter().map(String::len).sum::<usize>()
                            })
                            .sum::<usize>();
                    ensure!(
                        bytes <= gp64_dos::shell::MAX_BATCH_RETAINED_BYTES,
                        "batch memory limit exceeded"
                    );
                    // Keep this command's redirects on the suspended caller;
                    // the child inherits them until it returns to that frame.
                    self.parents.push((
                        std::mem::replace(&mut self.state, child),
                        std::mem::take(&mut self.pending),
                    ));
                } else {
                    return Ok(BatchAction::Launch {
                        program: program.trim_matches('"').to_owned(),
                        args: tail.to_owned(),
                    });
                }
            }
            "IF" => {
                let (first, tail) = split_command(rest);
                let (negate, condition, tail) = if first.eq_ignore_ascii_case("not") {
                    let (condition, tail) = split_command(tail);
                    (true, condition, tail)
                } else {
                    (false, first, tail)
                };
                let (met, command) = if condition.eq_ignore_ascii_case("errorlevel") {
                    let (threshold, command) = split_command(tail);
                    let (threshold, command) = if threshold == "==" {
                        split_command(command)
                    } else {
                        (threshold, command)
                    };
                    (
                        self.state.last_exit_code >= threshold.parse::<u8>()?,
                        command,
                    )
                } else if condition.eq_ignore_ascii_case("exist") {
                    let (file, command) = split_command(tail);
                    let path = dos.path_resolver.resolve(file.trim_matches('"'));
                    (
                        drive(dos, path.drive)?.get_attributes(&path.path).is_ok(),
                        command,
                    )
                } else if let Some((left, right)) = condition.split_once("==") {
                    (
                        if left.eq_ignore_ascii_case("ERRORLEVEL") {
                            self.state.last_exit_code >= right.parse::<u8>()?
                        } else {
                            left == right
                        },
                        tail,
                    )
                } else if split_command(tail).0 == "==" {
                    let (_, tail) = split_command(tail);
                    let (right, command) = split_command(tail);
                    (condition == right, command)
                } else {
                    bail!("unsupported IF condition: {rest}");
                };
                if met != negate {
                    self.pending.push_front(format!("@{command}"));
                }
            }
            "FOR" => {
                let (var, tail) = split_command(rest);
                let (keyword, tail) = split_command(tail);
                ensure!(
                    var.starts_with("%%") && keyword.eq_ignore_ascii_case("in"),
                    "invalid FOR syntax"
                );
                let tail = tail.trim_start();
                ensure!(tail.starts_with('('), "FOR requires a set");
                let close = tail
                    .find(')')
                    .ok_or_else(|| anyhow!("FOR requires a closing parenthesis"))?;
                let (keyword, command) = split_command(&tail[close + 1..]);
                ensure!(keyword.eq_ignore_ascii_case("do"), "FOR requires DO");
                let set = self
                    .state
                    .expand_variables(&tail[1..close], &environment(dos, machine));
                let items = tokens(&set.replace(',', " "));
                self.enqueue_for_commands(var, command, &items)?;
            }
            "DIR" => {
                let pattern = args
                    .iter()
                    .find(|a| !a.starts_with('/'))
                    .map_or("*.*", String::as_str);
                let resolved = dos.path_resolver.resolve(pattern);
                let (directory, wildcard) = resolved
                    .path
                    .rsplit_once('\\')
                    .unwrap_or(("\\", &resolved.path));
                let directory = if directory.is_empty() {
                    "\\"
                } else {
                    directory
                };
                let entries = drive(dos, resolved.drive)?.read_dir(directory)?;
                for (name, attrs) in entries {
                    if wildcard_match(&name, wildcard) {
                        if args.iter().any(|a| a.eq_ignore_ascii_case("/b")) {
                            output(dos, machine, &format!("{name}\r\n"))?;
                        } else {
                            output(
                                dos,
                                machine,
                                &format!(
                                    "{name:<13} {}\r\n",
                                    if attrs.directory {
                                        "<DIR>".to_owned()
                                    } else {
                                        attrs.size.to_string()
                                    }
                                ),
                            )?;
                        }
                    }
                }
            }
            _ if name.len() == 2
                && name.ends_with(':')
                && name.as_bytes()[0].is_ascii_alphabetic() =>
            {
                let drive = name.as_bytes()[0].to_ascii_uppercase() - b'A';
                ensure!(
                    dos.drives.contains_key(&drive),
                    "drive {name} is not mounted"
                );
                dos.path_resolver.set_current_drive(drive);
            }
            _ => {
                return Ok(BatchAction::Launch {
                    program: name.trim_matches('"').to_owned(),
                    args: rest.to_owned(),
                });
            }
        }
        Ok(BatchAction::Continue)
    }
}

fn copy_file_contents(
    source: &mut dyn gp64_dos::filesystem::VirtualFile,
    target: &mut dyn gp64_dos::filesystem::VirtualFile,
) -> Result<()> {
    let source_identity = source.host_identity()?;
    let target_identity = target.host_identity()?;
    if let (Some(source), Some(target)) = (source_identity, target_identity) {
        ensure!(source != target, "cannot copy a file onto itself");
    }
    target.set_len(0)?;
    std::io::copy(source, target)?;
    Ok(())
}

fn restore(dos: &mut gp64_dos::Dos, state: &mut BatchState) {
    while let Some(saved) = state.child_redirects.pop() {
        dos.restore_std_handle(saved);
    }
}
fn drive(
    dos: &mut gp64_dos::Dos,
    index: u8,
) -> Result<&mut (dyn gp64_dos::filesystem::VirtualDrive + 'static)> {
    dos.drives
        .get_mut(&index)
        .map(Box::as_mut)
        .ok_or_else(|| anyhow!("drive {} is not mounted", (b'A' + index) as char))
}
fn split_command(line: &str) -> (&str, &str) {
    let line = line.trim_matches(|c: char| c.is_ascii_whitespace());
    for keyword in ["echo", "chdir", "cd"] {
        if line
            .get(..keyword.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(keyword))
            && line
                .as_bytes()
                .get(keyword.len())
                .is_some_and(|b| matches!(b, b'.' | b'\\' | b'/'))
        {
            return (&line[..keyword.len()], &line[keyword.len()..]);
        }
    }
    let end = if let Some(tail) = line.strip_prefix('"') {
        tail.find('"').map_or(line.len(), |n| n + 2)
    } else {
        line.find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(line.len())
    };
    (
        &line[..end],
        line[end..].trim_start_matches(|c: char| c.is_ascii_whitespace()),
    )
}
fn tokens(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for ch in text.chars() {
        if ch == '"' {
            quoted = !quoted;
        } else if ch.is_ascii_whitespace() && !quoted {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn environment(dos: &gp64_dos::Dos, machine: &crate::Machine) -> HashMap<String, String> {
    let segment = dos.env_segment();
    let capacity = dos
        .dos_mem
        .block_end_segment(segment)
        .map_or(0, |end| usize::from(end.saturating_sub(segment)) * 16);
    let base = usize::from(segment) * 16;
    let mut result = HashMap::new();
    let mut bytes = Vec::new();
    for index in 0..capacity {
        let byte = machine.memory.read_u8(base + index);
        if byte == 0 {
            if bytes.is_empty() {
                break;
            }
            let entry = decode_dos_text(&bytes);
            if let Some((key, value)) = entry.split_once('=') {
                result.insert(key.to_ascii_uppercase(), value.to_owned());
            }
            bytes.clear();
        } else {
            bytes.push(byte);
        }
    }
    result
}
fn set_environment(
    dos: &gp64_dos::Dos,
    machine: &mut crate::Machine,
    name: &str,
    value: &str,
) -> Result<()> {
    let segment = dos.env_segment();
    let base = usize::from(segment) * 16;
    let capacity = dos
        .dos_mem
        .block_end_segment(segment)
        .map_or(0, |end| usize::from(end.saturating_sub(segment)) * 16);
    ensure!(capacity > 0, "DOS environment is not initialized");
    let mut env = environment(dos, machine);
    if value.is_empty() {
        env.remove(&name.to_ascii_uppercase());
    } else {
        env.insert(name.to_ascii_uppercase(), value.to_owned());
    }
    let original: Vec<u8> = (0..capacity)
        .map(|i| machine.memory.read_u8(base + i))
        .collect();
    let suffix_start = original
        .windows(2)
        .position(|w| w == [0, 0])
        .map_or(capacity, |n| n + 2);
    let mut entries: Vec<_> = env.into_iter().collect();
    entries.sort();
    let mut bytes = Vec::new();
    for (key, value) in entries {
        bytes.extend(encode_dos_text(&format!("{key}={value}")));
        bytes.push(0);
    }
    if bytes.is_empty() {
        bytes.push(0);
    }
    bytes.push(0);
    if suffix_start + 2 <= capacity {
        let name_end = original[suffix_start + 2..]
            .iter()
            .position(|&b| b == 0)
            .map_or(capacity, |n| suffix_start + 3 + n);
        bytes.extend_from_slice(&original[suffix_start..name_end]);
    }
    ensure!(bytes.len() <= capacity, "DOS environment block is full");
    machine.memory.load(base, &bytes)?;
    Ok(())
}
fn output(dos: &mut gp64_dos::Dos, machine: &mut crate::Machine, text: &str) -> Result<()> {
    // AH=40 preserves the kernel's stdout capture and standard-handle routing.
    let registers = machine.cpu.regs.clone();
    let base = 0x500;
    let saved: Vec<u8> = (0..256).map(|i| machine.memory.read_u8(base + i)).collect();
    let stack_base = 0x6efa;
    let saved_stack: Vec<u8> = (0..6)
        .map(|i| machine.memory.read_u8(stack_base + i))
        .collect();
    let result = (|| {
        for chunk in encode_dos_text(text).chunks(256) {
            machine.memory.load(base, chunk)?;
            machine.cpu.regs.set_ax(0x4000);
            machine.cpu.regs.set_bx(1);
            machine.cpu.regs.set_cx(chunk.len() as u16);
            machine.cpu.regs.ds = 0;
            machine.cpu.regs.set_dx(base as u16);
            // The copied DOS kernel updates the caller's INT frame flags.
            machine.cpu.regs.ss = 0;
            machine.cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(0);
            machine.cpu.regs.ds_cache = gp64_cpu::SegmentCache::real_mode(0);
            machine.cpu.regs.set_sp(0x6f00);
            machine
                .cpu
                .try_push16(&mut machine.memory, machine.cpu.regs.flags.bits())?;
            machine
                .cpu
                .try_push16(&mut machine.memory, machine.cpu.regs.cs)?;
            machine
                .cpu
                .try_push16(&mut machine.memory, machine.cpu.regs.ip())?;
            dos.int21(
                &mut machine.cpu,
                &mut machine.memory,
                &mut machine.io.vga,
                &mut machine.io.keyboard,
                &mut machine.io.pic,
            )?;
            ensure!(!machine.cpu.regs.flags.get_cf(), "DOS stdout write failed");
        }
        Ok(())
    })();
    machine.cpu.regs = registers;
    machine.memory.load(base, &saved)?;
    machine.memory.load(stack_base, &saved_stack)?;
    result
}
type Redirection = (u16, String, RedirectMode);

fn parse_redirects(line: &str) -> Result<(String, Vec<Redirection>)> {
    let mut command = String::new();
    let mut redirects = Vec::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        if ch == '"' {
            quoted = !quoted;
            command.push(ch);
        } else if !quoted && matches!(ch, '<' | '>') {
            let append = ch == '>' && chars.peek() == Some(&'>');
            if append {
                chars.next();
            }
            while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                chars.next();
            }
            let mut file = String::new();
            let file_quoted = chars.peek() == Some(&'"');
            if file_quoted {
                chars.next();
            }
            while let Some(&c) = chars.peek() {
                if file_quoted && c == '"' {
                    chars.next();
                    break;
                }
                if !file_quoted && (c.is_ascii_whitespace() || matches!(c, '<' | '>')) {
                    break;
                }
                file.push(c);
                chars.next();
            }
            ensure!(!file.is_empty(), "missing redirection target");
            redirects.push((
                if ch == '<' { 0 } else { 1 },
                file,
                if ch == '<' {
                    RedirectMode::Input
                } else if append {
                    RedirectMode::Append
                } else {
                    RedirectMode::Truncate
                },
            ));
            command.push(' ');
        } else {
            command.push(ch);
        }
    }
    Ok((command.trim().to_owned(), redirects))
}
fn wildcard_match(name: &str, pattern: &str) -> bool {
    if pattern == "*.*" {
        return true;
    }
    let name = name.to_ascii_uppercase();
    let pattern = pattern.to_ascii_uppercase();
    let (name, pattern) = (name.as_bytes(), pattern.as_bytes());
    let (mut n, mut p, mut star, mut retry) = (0, 0, None, 0);
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == name[n]) {
            n += 1;
            p += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = n;
        } else if let Some(s) = star {
            retry += 1;
            n = retry;
            p = s + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dos_with_environment(machine: &mut crate::Machine) -> gp64_dos::Dos {
        let mut dos = gp64_dos::Dos::new();
        dos.init_environment(&mut machine.memory, &gp64_types::SoundEnvConfig::default());
        dos
    }

    struct MemoryFile(std::io::Cursor<Vec<u8>>, bool);

    impl std::io::Read for MemoryFile {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buffer)
        }
    }

    impl std::io::Write for MemoryFile {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.0, buffer)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl std::io::Seek for MemoryFile {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            std::io::Seek::seek(&mut self.0, position)
        }
    }

    impl gp64_dos::filesystem::VirtualFile for MemoryFile {
        fn host_identity(&self) -> Result<Option<gp64_dos::filesystem::HostFileIdentity>> {
            if self.1 {
                bail!("generated identity failure");
            }
            Ok(None)
        }
        fn set_len(&mut self, size: u64) -> Result<()> {
            self.0.get_mut().resize(size as usize, 0);
            Ok(())
        }
        fn sync_all(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn copy_virtual_files_without_host_identity() {
        let mut source = MemoryFile(std::io::Cursor::new(b"data".to_vec()), false);
        let mut target = MemoryFile(
            std::io::Cursor::new(b"longer previous data".to_vec()),
            false,
        );
        copy_file_contents(&mut source, &mut target).unwrap();
        assert_eq!(source.0.into_inner(), b"data");
        assert_eq!(target.0.into_inner(), b"data");
    }

    #[test]
    fn copy_identity_error_preserves_destination() {
        for source_fails in [true, false] {
            let mut source = MemoryFile(std::io::Cursor::new(b"data".to_vec()), source_fails);
            let mut target = MemoryFile(
                std::io::Cursor::new(b"previous destination".to_vec()),
                !source_fails,
            );
            let result = copy_file_contents(&mut source, &mut target);
            assert_eq!(target.0.into_inner(), b"previous destination");
            assert_eq!(source.0.into_inner(), b"data");
            assert!(result.unwrap_err().to_string().contains("identity failure"));
        }
    }

    struct CopyDrive(std::path::PathBuf);

    impl CopyDrive {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let scratch =
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
            std::fs::create_dir_all(&scratch).unwrap();
            let root = scratch.join(format!(
                "batch-copy-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn dos(&self) -> gp64_dos::Dos {
            let mut dos = gp64_dos::Dos::new();
            dos.drives.clear();
            dos.drives.insert(
                2,
                Box::new(gp64_dos::filesystem::HostDrive::new(self.0.clone())),
            );
            dos
        }
    }

    impl Drop for CopyDrive {
        fn drop(&mut self) {
            for entry in std::fs::read_dir(&self.0).unwrap() {
                std::fs::remove_file(entry.unwrap().path()).unwrap();
            }
            std::fs::remove_dir(&self.0).unwrap();
        }
    }

    fn assert_copy_alias_preserved(root: &CopyDrive, from: &str, to: &str, subst: bool) {
        let bytes = b"generated COPY regression contents";
        std::fs::write(root.0.join(from), bytes).unwrap();
        let mut dos = root.dos();
        let mut machine = crate::Machine::new();
        if subst {
            let mut runner = BatchRunner::new("@SUBST D: C:\\", "test.bat", "");
            runner.next(&mut dos, &mut machine).unwrap();
        }
        let mut runner =
            BatchRunner::new(&format!("@COPY \"C:\\{from}\" \"{to}\""), "test.bat", "");
        let result = runner.next(&mut dos, &mut machine);
        assert_eq!(std::fs::read(root.0.join(from)).unwrap(), bytes);
        assert!(result.unwrap_err().to_string().contains("onto itself"));
    }

    #[test]
    fn copy_subst_alias_preserves_source() {
        assert_copy_alias_preserved(&CopyDrive::new(), "SAVE.TXT", "D:\\SAVE.TXT", true);
    }

    #[test]
    fn copy_case_alias_preserves_source() {
        assert_copy_alias_preserved(&CopyDrive::new(), "Save.txt", "D:\\SAVE.TXT", true);
    }

    #[test]
    fn copy_short_name_alias_preserves_source() {
        let root = CopyDrive::new();
        let long = "Generated Long Filename.txt";
        std::fs::write(root.0.join(long), b"generated").unwrap();
        let short = gp64_dos::sfn::assign_short_names_in_dir(Some(&root.0), &[long.into()])
            .pop()
            .unwrap()
            .1;
        assert_copy_alias_preserved(&root, long, &format!("C:\\{short}"), false);
    }

    #[test]
    fn copy_hardlink_alias_preserves_source() {
        let root = CopyDrive::new();
        std::fs::write(root.0.join("SAVE.TXT"), b"generated").unwrap();
        std::fs::hard_link(root.0.join("SAVE.TXT"), root.0.join("ALIAS.TXT")).unwrap();
        assert_copy_alias_preserved(&root, "SAVE.TXT", "C:\\ALIAS.TXT", false);
    }

    #[cfg(unix)]
    #[test]
    fn copy_symlink_alias_preserves_source() {
        let root = CopyDrive::new();
        std::os::unix::fs::symlink("SAVE.TXT", root.0.join("ALIAS.TXT")).unwrap();
        assert_copy_alias_preserved(&root, "SAVE.TXT", "C:\\ALIAS.TXT", false);
    }

    #[test]
    fn copy_distinct_file_creates_and_truncates_destination() {
        let root = CopyDrive::new();
        std::fs::write(root.0.join("SAVE.TXT"), b"data").unwrap();
        let mut dos = root.dos();
        let mut machine = crate::Machine::new();
        for old in [None, Some(b"longer destination contents".as_slice())] {
            if let Some(old) = old {
                std::fs::write(root.0.join("TARGET.TXT"), old).unwrap();
            }
            let mut runner = BatchRunner::new("@COPY C:\\SAVE.TXT C:\\TARGET.TXT", "test.bat", "");
            runner.next(&mut dos, &mut machine).unwrap();
            assert_eq!(std::fs::read(root.0.join("TARGET.TXT")).unwrap(), b"data");
            assert_eq!(std::fs::read(root.0.join("SAVE.TXT")).unwrap(), b"data");
        }
    }

    #[test]
    fn shell_output_preserves_ivt_and_keyboard_stubs() {
        let mut dos = gp64_dos::Dos::new();
        let mut machine = crate::Machine::new();
        gp64_bios::Bios::new().init_ivt(&mut machine.memory, false);
        machine.memory.write_u16(4, 0xffff);
        let before = machine.memory.ram[..0x600].to_vec();
        dos.enable_stdout_capture();
        output(&mut dos, &mut machine, "shell output").unwrap();
        assert_eq!(&machine.memory.ram[..0x600], before);
        assert_eq!(dos.take_stdout_capture(), b"shell output");
    }

    #[test]
    fn batch_resumes_after_program_and_tests_errorlevel() {
        let mut runner = BatchRunner::new(
            "@echo off\nchild /test\nif errorlevel 3 echo passed\nexit /b 7",
            "script.bat",
            "",
        );
        let mut dos = gp64_dos::Dos::new();
        let mut machine = crate::Machine::new();
        dos.enable_stdout_capture();
        assert_eq!(
            runner.next(&mut dos, &mut machine).unwrap(),
            BatchAction::Continue
        );
        assert_eq!(
            runner.next(&mut dos, &mut machine).unwrap(),
            BatchAction::Launch {
                program: "child".into(),
                args: "/test".into()
            }
        );
        assert_eq!(
            runner.next(&mut dos, &mut machine).unwrap(),
            BatchAction::Waiting
        );
        runner.program_exited(3);
        for _ in 0..3 {
            assert_eq!(
                runner.next(&mut dos, &mut machine).unwrap(),
                BatchAction::Continue
            );
        }
        assert_eq!(
            runner.next(&mut dos, &mut machine).unwrap(),
            BatchAction::Finished(7)
        );
        assert_eq!(dos.take_stdout_capture(), b"passed\r\n");
    }

    #[test]
    fn for_budget_rejects_amplification_before_materialization() {
        let item = "x".repeat(2048);
        let set = vec![item.as_str(); 128].join(" ");
        let body = "%%x".repeat(128);
        let script = format!("@for %%x in ({set}) do {body}");
        assert!(script.len() < gp64_dos::shell::MAX_BATCH_FILE_BYTES as usize);
        let mut runner = BatchRunner::new(&script, "script.bat", "");
        let pending = VecDeque::from(["@echo preserved".into()]);
        runner.pending = std::iter::once(script).chain(pending.clone()).collect();
        let mut dos = gp64_dos::Dos::new();
        let mut machine = crate::Machine::new();
        FOR_MATERIALIZATIONS.with(|count| count.set(0));
        assert_eq!(
            runner.next(&mut dos, &mut machine).unwrap_err().to_string(),
            "FOR pending commands exceed batch limit"
        );
        assert_eq!(runner.pending, pending);
        assert_eq!(FOR_MATERIALIZATIONS.with(|count| count.get()), 0);

        // The reported shape repeats a large constant body for tiny items.
        let items = vec!["x".to_owned(); 4096];
        let command = format!("echo {}", "a".repeat(900 << 10));
        let script = format!("@for %%x in ({}) do {command}", items.join(" "));
        assert!(script.len() < gp64_dos::shell::MAX_BATCH_FILE_BYTES as usize);
        assert!(runner
            .enqueue_for_commands("%%x", &command, &items)
            .is_err());
        assert_eq!(FOR_MATERIALIZATIONS.with(|count| count.get()), 0);
        assert_eq!(runner.pending, pending);

        // A sub-1MiB shape whose repeated replacements would exceed 3GiB.
        // Only the preflight runs: no expanded command is constructed.
        let items = vec!["x".repeat(193); 4096];
        let command = "%%x".repeat(4096);
        assert!(
            items.iter().map(String::len).sum::<usize>() + items.len() + command.len() < (1 << 20)
        );
        assert!(4096usize * 193 * 4096 > (3usize << 30));
        assert!(runner
            .enqueue_for_commands("%%x", &command, &items)
            .is_err());
        assert_eq!(FOR_MATERIALIZATIONS.with(|count| count.get()), 0);
        assert_eq!(runner.pending, pending);
    }

    #[test]
    fn for_budget_counts_all_frames_and_accepts_exact_byte_boundary() {
        let mut runner = BatchRunner::new("source", "script.bat", "argument");
        runner.pending.push_back("@echo active".into());
        for name in ["first", "second"] {
            runner.parents.push((
                BatchState::new(name, &[format!("{name}.bat"), "arg".into()]),
                VecDeque::from([format!("@echo {name}")]),
            ));
        }
        let items = vec!["é".repeat(4096), "long replacement".into()];
        let command = "echo %%x%%x%%xy%%x";
        let expected: VecDeque<_> = items
            .iter()
            .map(|item| format!("@{}", command.replace("%%x", item)))
            .collect();
        let header = std::mem::size_of::<String>();
        let retained = runner.state.retained_bytes()
            + runner
                .pending
                .iter()
                .map(|text| text.len() + header)
                .sum::<usize>()
            + runner
                .parents
                .iter()
                .map(|(state, pending)| {
                    state.retained_bytes()
                        + pending
                            .iter()
                            .map(|text| text.len() + header)
                            .sum::<usize>()
                })
                .sum::<usize>();
        let generated = expected
            .iter()
            .map(|text| text.len() + header)
            .sum::<usize>();
        let padding = gp64_dos::shell::MAX_BATCH_RETAINED_BYTES - retained - generated - header;
        runner.state.lines.push("p".repeat(padding));
        let pending = runner.pending.clone();
        FOR_MATERIALIZATIONS.with(|count| count.set(0));
        runner.enqueue_for_commands("%%x", command, &items).unwrap();
        assert_eq!(FOR_MATERIALIZATIONS.with(|count| count.get()), items.len());
        assert_eq!(
            runner.pending,
            expected
                .into_iter()
                .chain(pending.clone())
                .collect::<VecDeque<_>>()
        );
        runner.pending = pending.clone();
        runner.state.lines.last_mut().unwrap().push('p');
        FOR_MATERIALIZATIONS.with(|count| count.set(0));
        assert!(runner.enqueue_for_commands("%%x", command, &items).is_err());
        assert_eq!(FOR_MATERIALIZATIONS.with(|count| count.get()), 0);
        assert_eq!(runner.pending, pending);
    }

    #[test]
    fn for_budget_preserves_active_pending_count_boundary() {
        let mut runner = BatchRunner::new("", "script.bat", "");
        // The count limit applies to the active queue, as before.
        runner.parents.push((
            BatchState::new("", &[]),
            VecDeque::from(vec![String::new(); 4096]),
        ));
        let items = vec!["one".into(); 4096];
        runner
            .enqueue_for_commands("%%x", "echo %%x", &items)
            .unwrap();
        assert_eq!(runner.pending.len(), 4096);
        assert!(runner.pending.iter().all(|text| text == "@echo one"));
        let pending = runner.pending.clone();
        FOR_MATERIALIZATIONS.with(|count| count.set(0));
        assert!(runner
            .enqueue_for_commands("%%x", "echo %%x", &["two".into()])
            .is_err());
        assert_eq!(FOR_MATERIALIZATIONS.with(|count| count.get()), 0);
        assert_eq!(runner.pending, pending);
        runner.pending.clear();
        assert_eq!(
            runner
                .enqueue_for_commands("%%x", "echo %%x", &vec!["one".into(); 4097])
                .unwrap_err()
                .to_string(),
            "FOR set too large"
        );
        assert!(runner.pending.is_empty());
        assert_eq!(FOR_MATERIALIZATIONS.with(|count| count.get()), 0);
    }

    #[test]
    fn for_yields_each_program_and_goto_does_not_grow_frames() {
        let mut runner = BatchRunner::new(
            "@echo off\nfor %%x in (one two) do child %%x\n:again\nif a==a goto again",
            "script.bat",
            "",
        );
        let mut dos = gp64_dos::Dos::new();
        let mut machine = crate::Machine::new();
        runner.next(&mut dos, &mut machine).unwrap();
        runner.next(&mut dos, &mut machine).unwrap();
        for arg in ["one", "two"] {
            assert_eq!(
                runner.next(&mut dos, &mut machine).unwrap(),
                BatchAction::Launch {
                    program: "child".into(),
                    args: arg.into()
                }
            );
            runner.program_exited(0);
        }
        let lines = runner.state.lines.len();
        for _ in 0..100 {
            assert_eq!(
                runner.next(&mut dos, &mut machine).unwrap(),
                BatchAction::Continue
            );
        }
        assert_eq!(runner.state.lines.len(), lines);
        assert!(runner.pending.len() <= 1);
    }

    #[test]
    fn for_expands_environment_and_positional_items_before_tokenizing() {
        let script = decode_dos_text(
            b"@echo off\r\nset ITEMS=one two\r\nfor %%x in (%ITEMS%) do echo %%x\r\nset QUOTED=\"one two\" \"\x82cho\"\r\nfor %%x in (%QUOTED% %1) do echo [%%x][%%x]\r\n",
        );
        let mut runner = BatchRunner::new(&script, "script.bat", "three");
        let mut machine = crate::Machine::new();
        let mut dos = dos_with_environment(&mut machine);
        dos.enable_stdout_capture();

        for _ in 0..16 {
            if runner.next(&mut dos, &mut machine).unwrap() == BatchAction::Finished(0) {
                assert_eq!(
                    dos.take_stdout_capture(),
                    b"one\r\ntwo\r\n[one two][one two]\r\n[\x82cho][\x82cho]\r\n[three][three]\r\n"
                );
                return;
            }
        }
        panic!("FOR batch did not finish");
    }

    #[test]
    fn for_empty_and_missing_item_sets_do_not_run_the_body() {
        let mut runner = BatchRunner::new(
            "@echo off\nset EMPTY=\nfor %%x in (%EMPTY%) do echo unexpected\nfor %%x in (%MISSING%) do echo unexpected\necho done",
            "script.bat",
            "",
        );
        let mut machine = crate::Machine::new();
        let mut dos = dos_with_environment(&mut machine);
        dos.enable_stdout_capture();

        for _ in 0..10 {
            if runner.next(&mut dos, &mut machine).unwrap() == BatchAction::Finished(0) {
                assert_eq!(dos.take_stdout_capture(), b"done\r\n");
                return;
            }
        }
        panic!("FOR batch did not finish");
    }

    #[test]
    fn for_expands_body_variables_when_each_iteration_runs() {
        let mut runner = BatchRunner::new(
            "@echo off\nset ACC=\nfor %%x in (one two) do set ACC=%ACC%%%x\necho %ACC%",
            "script.bat",
            "",
        );
        let mut machine = crate::Machine::new();
        let mut dos = dos_with_environment(&mut machine);
        dos.enable_stdout_capture();

        for _ in 0..10 {
            if runner.next(&mut dos, &mut machine).unwrap() == BatchAction::Finished(0) {
                assert_eq!(dos.take_stdout_capture(), b"onetwo\r\n");
                return;
            }
        }
        panic!("FOR batch did not finish");
    }

    #[test]
    fn goto_abandons_for_commands_only_after_success() {
        let mut runner = BatchRunner::new(
            "@echo off\nfor %%x in (missing one two) do goto %%x\n:one\n@echo one\ngoto :eof\n:two\n@echo two",
            "script.bat",
            "",
        );
        let mut dos = gp64_dos::Dos::new();
        dos.drives.clear();
        let mut machine = crate::Machine::new();
        dos.enable_stdout_capture();
        runner.next(&mut dos, &mut machine).unwrap();
        runner.next(&mut dos, &mut machine).unwrap();
        let pending = runner
            .pending
            .iter()
            .skip(1)
            .cloned()
            .collect::<VecDeque<_>>();
        let line = runner.state.current_line;
        assert_eq!(
            runner.next(&mut dos, &mut machine).unwrap_err().to_string(),
            "label not found: missing"
        );
        assert_eq!(runner.pending, pending);
        assert_eq!(runner.state.current_line, line);
        for _ in 0..20 {
            if runner.next(&mut dos, &mut machine).unwrap() == BatchAction::Finished(0) {
                assert_eq!(dos.take_stdout_capture(), b"one\r\n");
                return;
            }
        }
        panic!("GOTO batch did not finish");
    }

    #[test]
    fn if_generated_goto_abandons_for_commands() {
        let mut runner = BatchRunner::new(
            "@echo off\nfor %%x in (one two) do if a==a goto %%x\n:one\n@echo one\ngoto :eof\n:two\n@echo two",
            "script.bat",
            "",
        );
        let mut dos = gp64_dos::Dos::new();
        dos.drives.clear();
        let mut machine = crate::Machine::new();
        dos.enable_stdout_capture();
        for _ in 0..20 {
            if runner.next(&mut dos, &mut machine).unwrap() == BatchAction::Finished(0) {
                assert_eq!(dos.take_stdout_capture(), b"one\r\n");
                return;
            }
        }
        panic!("IF GOTO batch did not finish");
    }

    #[test]
    fn goto_abandons_nested_for_commands_but_preserves_nested_callers() {
        let root = CopyDrive::new();
        std::fs::write(
            root.0.join("outer.bat"),
            "@echo off\nfor %%y in (alpha beta) do call inner.bat\n@echo outer done\ngoto :eof",
        )
        .unwrap();
        std::fs::write(
            root.0.join("inner.bat"),
            "@echo off\nfor %%x in (one two) do for %%z in (first second) do goto %%x\n:one\n@echo inner one\ngoto :eof\n:two\n@echo inner two",
        )
        .unwrap();
        let mut runner = BatchRunner::new(
            "@echo off\nfor %%x in (first second) do call outer.bat\n@echo main done\ngoto :eof",
            "script.bat",
            "",
        );
        let mut dos = root.dos();
        let mut machine = crate::Machine::new();
        dos.enable_stdout_capture();
        for _ in 0..100 {
            if runner.next(&mut dos, &mut machine).unwrap() == BatchAction::Finished(0) {
                assert_eq!(
                    dos.take_stdout_capture(),
                    b"inner one\r\ninner one\r\nouter done\r\ninner one\r\ninner one\r\nouter done\r\nmain done\r\n"
                );
                assert!(runner.parents.is_empty());
                assert!(runner.pending.is_empty());
                return;
            }
        }
        panic!("nested CALL/FOR batch did not finish");
    }

    // Independently assembled COM fixtures exercise the real DOS services.
    fn write_com(handle: u16, text: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xb8, 0x00, 0x40, 0xbb]; // AX=4000h; BX=handle
        bytes.extend(handle.to_le_bytes());
        bytes.push(0xb9); // CX=length
        bytes.extend((text.len() as u16).to_le_bytes());
        bytes.push(0xba); // DX=text at COM offset 100h+19
        bytes.extend(0x113u16.to_le_bytes());
        bytes.extend([0xcd, 0x21, 0xb8, 0, 0x4c, 0xcd, 0x21]);
        assert_eq!(bytes.len(), 19);
        bytes.extend(text);
        bytes
    }

    fn read_three_com() -> Vec<u8> {
        // Read three bytes from stdin, write the returned byte count to stdout.
        let mut bytes = vec![
            0xb4, 0x3f, 0x31, 0xdb, 0xb9, 3, 0, 0xba, 0, 0, 0xcd, 0x21, 0x89, 0xc1, 0xbb, 1, 0,
            0xb4, 0x40, 0xcd, 0x21, 0xb8, 0, 0x4c, 0xcd, 0x21,
        ];
        let pointer = 0x100 + bytes.len() as u16;
        bytes[8..10].copy_from_slice(&pointer.to_le_bytes());
        bytes.extend([0; 3]);
        bytes
    }

    fn finish_call(pc: &mut crate::DosMachine, code: u8) {
        for _ in 0..100 {
            match pc.run_for(100_000).unwrap() {
                crate::DosRunExit::Running => {}
                crate::DosRunExit::Exited(actual) => {
                    assert_eq!(actual, code);
                    return;
                }
                other => panic!("unexpected CALL fixture stop: {other:?}"),
            }
        }
        panic!("CALL fixture did not finish");
    }

    #[test]
    fn call_redirect_keeps_internal_and_external_output_until_return() {
        let root = CopyDrive::new();
        std::fs::write(root.0.join("WRITE.COM"), write_com(1, b"external\r\n")).unwrap();
        std::fs::write(
            root.0.join("CHILD.BAT"),
            "@ECHO child\n@WRITE.COM\n@ECHO last",
        )
        .unwrap();
        std::fs::write(
            root.0.join("MAIN.BAT"),
            "@CALL CHILD.BAT > OUT.TXT\n@ECHO parent",
        )
        .unwrap();
        let mut pc = crate::DosMachine::launch(&root.0, "MAIN.BAT", "").unwrap();
        finish_call(&mut pc, 0);
        assert_eq!(
            std::fs::read(root.0.join("OUT.TXT")).unwrap(),
            b"child\r\nexternal\r\nlast\r\n"
        );
        assert_eq!(pc.take_console_output(), b"parent\r\n");
        assert!((0..256).all(|handle| pc.dos.file_table.get_sft_index(handle).is_none()));
    }

    #[test]
    fn call_redirect_nested_restores_stdout_stdin_and_stderr_lifo() {
        let root = CopyDrive::new();
        for (name, bytes) in [
            ("READ.COM", read_three_com()),
            ("ERROR.COM", write_com(2, b"stderr\r\n")),
            ("INPUT.TXT", b"abcdef".to_vec()),
            ("ALT.TXT", b"XYZ".to_vec()),
            ("MAIN.BAT", b"@CALL OUTER.BAT > OUT.TXT < INPUT.TXT\n@ECHO parent".to_vec()),
            ("OUTER.BAT", b"@ECHO outer\n@READ.COM\n@ECHO local > LOCAL.TXT\n@CALL INNER.BAT >> INNER.TXT < ALT.TXT\n@READ.COM\n@ECHO returned\n@EXIT /B 7".to_vec()),
            ("INNER.BAT", b"@ECHO inner\n@READ.COM\n@ERROR.COM\n@READ.COM > EMPTY.TXT\n@ECHO end\n@GOTO :EOF\n@ECHO unreachable".to_vec()),
            ("INNER.TXT", b"existing\r\n".to_vec()),
        ] {
            std::fs::write(root.0.join(name), bytes).unwrap();
        }
        let mut pc = crate::DosMachine::launch(&root.0, "MAIN.BAT", "").unwrap();
        for _ in 0..2 {
            assert!(matches!(
                pc.run_for(100_000).unwrap(),
                crate::DosRunExit::Running
            ));
        }
        assert_eq!(std::fs::read(root.0.join("OUT.TXT")).unwrap(), b"outer\r\n");
        finish_call(&mut pc, 7);
        assert_eq!(
            std::fs::read(root.0.join("OUT.TXT")).unwrap(),
            b"outer\r\nabcdefreturned\r\n"
        );
        assert_eq!(
            std::fs::read(root.0.join("INNER.TXT")).unwrap(),
            b"existing\r\ninner\r\nXYZend\r\n"
        );
        assert_eq!(
            std::fs::read(root.0.join("LOCAL.TXT")).unwrap(),
            b"local\r\n"
        );
        assert_eq!(std::fs::read(root.0.join("EMPTY.TXT")).unwrap(), b"");
        assert_eq!(pc.take_console_output(), b"stderr\r\nparent\r\n");
        assert!((0..256).all(|handle| pc.dos.file_table.get_sft_index(handle).is_none()));
    }

    #[test]
    fn call_redirect_unwinds_when_external_program_cannot_start() {
        for program in ["MISSING.COM", "BAD.EXE"] {
            let root = CopyDrive::new();
            std::fs::write(root.0.join("BAD.EXE"), b"MZ").unwrap();
            std::fs::write(
                root.0.join("CHILD.BAT"),
                format!("@ECHO child\n@{program} > LOCAL.TXT"),
            )
            .unwrap();
            std::fs::write(
                root.0.join("MAIN.BAT"),
                "@CALL CHILD.BAT > OUT.TXT\n@ECHO unreachable",
            )
            .unwrap();
            let mut pc = crate::DosMachine::launch(&root.0, "MAIN.BAT", "").unwrap();
            let initial = handle_bindings(&pc.dos);
            for _ in 0..2 {
                assert!(matches!(
                    pc.run_for(100_000).unwrap(),
                    crate::DosRunExit::Running
                ));
            }
            assert_eq!(
                std::fs::read(root.0.join("OUT.TXT")).unwrap(),
                b"child\r\n",
                "{program}"
            );
            assert!(pc.run_for(100_000).is_err(), "{program}");
            assert_eq!(handle_bindings(&pc.dos), initial, "{program}");
            output(&mut pc.dos, &mut pc.machine, "restored").unwrap();
            assert_eq!(pc.take_console_output(), b"restored");
            assert_eq!(std::fs::read(root.0.join("LOCAL.TXT")).unwrap(), b"");
        }
    }

    fn handle_bindings(dos: &gp64_dos::Dos) -> Vec<(Option<usize>, Option<u16>, bool)> {
        (0..256)
            .map(|handle| {
                let index = dos.file_table.get_sft_index(handle);
                (
                    index,
                    index.and_then(|i| dos.file_table.get_sft_ref_count(i)),
                    dos.file_table.is_handle_free(handle),
                )
            })
            .collect()
    }

    #[test]
    fn call_redirect_unwinds_all_saved_handles_on_errors_and_global_exit() {
        for terminal in [
            "@EXIT 9 > COMMAND.TXT",
            "@CD MISSING > COMMAND.TXT",
            "@ECHO bad > COMMAND.TXT < MISSING.TXT",
            "@ECHO bad >",
        ] {
            let root = CopyDrive::new();
            std::fs::write(root.0.join("INPUT.TXT"), b"input").unwrap();
            std::fs::write(root.0.join("INNER.BAT"), format!("@ECHO inner\n{terminal}")).unwrap();
            std::fs::write(
                root.0.join("OUTER.BAT"),
                "@CALL INNER.BAT > INNER.TXT < INPUT.TXT\n@ECHO unreachable",
            )
            .unwrap();
            let mut dos = root.dos();
            let mut machine = crate::Machine::new();
            let saved = dos
                .redirect_std_handle(1, "BASE.TXT", RedirectMode::Truncate)
                .unwrap();
            let input = dos
                .redirect_std_handle(0, "INPUT.TXT", RedirectMode::Input)
                .unwrap();
            // A stderr duplicate keeps the original stdout SFT alive during CALL.
            assert!(dos.file_table.force_duplicate(1, 2, dos.current_psp()));
            let initial = handle_bindings(&dos);
            let mut runner = BatchRunner::new(
                "@CALL OUTER.BAT > OUT.TXT < INPUT.TXT\n@ECHO unreachable",
                "MAIN.BAT",
                "",
            );
            for _ in 0..3 {
                assert_eq!(
                    runner.next(&mut dos, &mut machine).unwrap(),
                    BatchAction::Continue
                );
            }
            assert_eq!(
                std::fs::read(root.0.join("INNER.TXT")).unwrap(),
                b"inner\r\n",
                "{terminal}"
            );
            let result = runner.next(&mut dos, &mut machine);
            if terminal.starts_with("@EXIT") {
                assert_eq!(result.unwrap(), BatchAction::Continue);
                assert_eq!(
                    runner.next(&mut dos, &mut machine).unwrap(),
                    BatchAction::Finished(9)
                );
                assert!(runner.parents.is_empty());
            } else {
                assert!(result.is_err(), "{terminal}");
                assert_eq!(runner.parents.len(), 2);
            }
            assert_eq!(handle_bindings(&dos), initial, "{terminal}");
            output(&mut dos, &mut machine, "restored").unwrap();
            assert_eq!(std::fs::read(root.0.join("BASE.TXT")).unwrap(), b"restored");
            assert_eq!(
                std::fs::read(root.0.join("INNER.TXT")).unwrap(),
                b"inner\r\n"
            );
            dos.restore_std_handle(input);
            dos.restore_std_handle(saved);
        }
    }

    #[test]
    fn quoted_redirections_and_wildcards() {
        let (line, redirects) = parse_redirects("echo \"a > b\" >> \"out file\" < input").unwrap();
        assert_eq!(line, "echo \"a > b\"");
        assert_eq!(
            redirects,
            vec![
                (1, "out file".into(), RedirectMode::Append),
                (0, "input".into(), RedirectMode::Input)
            ]
        );
        assert!(wildcard_match("README", "*.*"));
        assert!(wildcard_match("test.txt", "t?st.*"));
        assert!(!wildcard_match("test.txt", "*.bat"));
    }
}
