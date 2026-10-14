//! HLD §10 Oracle 2, "No whole-slice regression" (U2): guest RAM is a sparse
//! host reservation, so no code outside `ram_backing.rs` may rebuild a slice
//! over it with `slice::from_raw_parts[_mut]`. RAM reaches other code only as
//! checked ranges (`ram[a..b]`, `ram.get(..)`) or as raw pointers from
//! `RamBacking::host_base`/`ptr_for`, which must not be turned back into a
//! slice. The compile-time half (no `Deref` to `[u8]`) is the `compile_fail`
//! doctests on `RamBacking`.

use std::path::{Path, PathBuf};

/// Whether an identifier names guest RAM or its base pointer.
fn is_ram_identifier(ident: &str) -> bool {
    ident == "ram"
        || ident.starts_with("ram_")
        || ident.ends_with("_ram")
        || ident == "host_base"
        || ident == "guest_ram"
}

/// The argument text of a `from_raw_parts[_mut](` call starting at `open`
/// (the index of its `(`): up to the matching `)`.
fn call_arguments(text: &str, open: usize) -> &str {
    let mut depth = 0usize;
    for (offset, ch) in text[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open + 1..open + offset];
                }
            }
            _ => {}
        }
    }
    &text[open + 1..]
}

/// Every `from_raw_parts` call in `text` whose arguments mention RAM, as
/// `(line, arguments)`.
fn ram_slice_rebuilds(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut search = 0;
    while let Some(hit) = text[search..].find("from_raw_parts") {
        let at = search + hit;
        search = at + "from_raw_parts".len();
        let rest = &text[search..];
        let rest = rest.strip_prefix("_mut").unwrap_or(rest);
        let Some(paren) = rest.find('(') else {
            continue;
        };
        if !rest[..paren].trim().is_empty() {
            continue;
        }
        let open = text.len() - rest.len() + paren;
        let args = call_arguments(text, open);
        let mentions_ram = args
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(is_ram_identifier);
        if mentions_ram {
            let line = text[..at].matches('\n').count() + 1;
            found.push((line, args.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/gp64-memory sits two levels below the workspace")
        .to_path_buf()
}

#[test]
fn no_code_outside_ram_backing_rebuilds_a_slice_over_guest_ram() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    let crates = std::fs::read_dir(root.join("crates")).expect("crates directory");
    for krate in crates.flatten() {
        // Every crate that can reach `Memory::ram`: it names this crate or the
        // root `gp64` package in its manifest.
        let manifest = std::fs::read_to_string(krate.path().join("Cargo.toml")).unwrap_or_default();
        let reaches_ram = manifest.contains("gp64-memory")
            || manifest
                .lines()
                .any(|l| l.starts_with("gp64 =") || l.starts_with("gp64.workspace"));
        if reaches_ram {
            rust_files(&krate.path().join("src"), &mut files);
            rust_files(&krate.path().join("tests"), &mut files);
        }
    }
    assert!(
        files.len() > 100,
        "the scan found only {} files",
        files.len()
    );

    let backing = root.join("crates/gp64-memory/src/ram_backing");
    let mut offenders = Vec::new();
    for file in files {
        if file.starts_with(&backing)
            || file.ends_with("gp64-memory/src/ram_backing.rs")
            || file.ends_with("gp64-memory/tests/ram_whole_slice_guard.rs")
        {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read source");
        for (line, args) in ram_slice_rebuilds(&text) {
            offenders.push(format!("{}:{line}: from_raw_parts({args})", file.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "slices rebuilt over guest RAM outside ram_backing.rs (use checked ranges \
         or RamBacking::get):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_scanner_flags_ram_rebuilds_and_ignores_other_slices() {
    let flagged = ram_slice_rebuilds(
        "let a = unsafe { std::slice::from_raw_parts(ram_ptr, ram_len) };\n\
         let b = unsafe { core::slice::from_raw_parts_mut(\n    mem.ram.host_base(),\n    n,\n) };\n",
    );
    assert_eq!(flagged.len(), 2, "{flagged:?}");
    assert_eq!(flagged[0].0, 1);
    assert_eq!(flagged[1].0, 2);
    let clean = ram_slice_rebuilds(
        "let c = unsafe { std::slice::from_raw_parts(fetch_base.add(pos), 15) };\n\
         let d = unsafe { std::slice::from_raw_parts_mut(&mut self.system as *mut u8, 23) };\n\
         let e = unsafe { std::slice::from_raw_parts(program.as_ptr(), frame_len) };\n",
    );
    assert!(clean.is_empty(), "{clean:?}");
}
