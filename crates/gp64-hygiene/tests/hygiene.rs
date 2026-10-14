//! Publication checks produce review candidates, not proof of provenance.
//! Owned inputs stay in memory; diagnostics contain only our paths and offsets.
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

// Split spellings keep the check's own definitions from matching themselves.
const WORDS: &[&str] = &[
    concat!("f1", "gp"),
    concat!("formula", " one"),
    concat!("formula", " 1"),
    concat!("grand", " prix"),
    concat!("micro", "prose"),
    concat!("cram", "mond"),
    concat!("gp", "rix"),
    concat!("mi", "1t"),
    concat!("rnc", " archive"),
];
// These two INTERNAL planning files are deliberately allowed whole. Arthur
// decides whether they ship in a public copy; no other directory is exempt.
const PLANNING: &[&str] = &[
    "wrk_docs/2026.10.10 - HLD - gp64 v1 cut-down DOS emulator.md",
    "wrk_journals/2026.10.10 - JRN - gp64 v1 build.md",
];
// Exact path:line-content, not line numbers (which drift), words or files.
const WORD_ALLOWLIST: &[&str] = &[
    concat!("crates/gp64-machine/src/launcher.rs:pub const DEFAULT_BATCH: &str = \"F1", "GP.BAT\";"),
    "crates/gp64-machine/src/launcher.rs:pub const DEFAULT_PROGRAM: &str = \"GP.EXE\";",
    concat!("crates/gp64-machine/src/launcher.rs:pub const MISSING_DEFAULT: &str = \"no F1", "GP.BAT or GP.EXE beside gp64\";"),
    concat!("README.md:Run Geoff Cram", "mond's original Formula", " One Grand", " Prix (F1", "GP) on Windows 11 and macOS."),
    concat!("README.md:Copy gp64 into your own F1", "GP folder and run it. It starts the normal batch launcher, or the executable if the batch file is absent."),
    concat!("README.md:No game files are included. You must own the game. gp64 is not affiliated with or endorsed by the rights holders of F1", "GP (Formula", " One Grand", " Prix)."),
];
// This exact historical line describes only the launcher defaults. The lead
// explicitly reviewed and allowed it; other commits and edited lines still fail.
const COMMIT_WORD_ALLOWLIST: &[&str] = &[
    concat!("commit/617485e633b212865b3b18d15ce16bea98a25d0a:The drop-in launcher runs ", "F1", "GP", ".BAT if present, else GP.EXE; those two names live only in launcher.rs. An owned test, ignored without GP64_GAME_DIR, launches the default headless at 20M cycles/s. It checks that both batch programs start, that no exception reaches an uninstalled vector, that a steady non-blank graphics frame appears, and that a key press changes the frame. Generic properties only."),
    // Pushed 2026-10-14 before the rule was noticed; they cannot be reworded.
    concat!("commit/141d891b277be27c7cc8dba022fc99f36be87311:thread at frame boundaries, so mdem's existing ", "F1", "GP", " .emu scripts drive"),
    concat!("commit/83cf2517b7cf309a3419bc7e66d18ea137b72598:as ", "F1", "GP", " opening its modem screen, the guest stayed stuck near 15 MHz"),
    concat!("commit/79693562fce87bac08046e9b84ca3b4fbaba3241:interpreter run. ", "F1", "GP", "'s screen code makes far calls in a hot loop, and"),
];
// The application icon master is generated artwork Arthur chose; the sized
// icons are shrunk from it.
const BINARY_ALLOWLIST: &[(&str, &str)] = &[
    ("crates/gp64/assets/icon.png", "crates/gp64/assets/icon.png"),
    (
        "crates/gp64/assets/icon-16.png",
        "crates/gp64/assets/icon.png",
    ),
    (
        "crates/gp64/assets/icon-24.png",
        "crates/gp64/assets/icon.png",
    ),
    (
        "crates/gp64/assets/icon-32.png",
        "crates/gp64/assets/icon.png",
    ),
    (
        "crates/gp64/assets/icon-48.png",
        "crates/gp64/assets/icon.png",
    ),
    (
        "crates/gp64/assets/icon-64.png",
        "crates/gp64/assets/icon.png",
    ),
    (
        "crates/gp64/assets/icon-256.png",
        "crates/gp64/assets/icon.png",
    ),
    (
        "crates/gp64-machine/tests/serial/answer-auto.com",
        "crates/gp64-machine/tests/serial/sertest.asm",
    ),
    (
        "crates/gp64-machine/tests/serial/answer.com",
        "crates/gp64-machine/tests/serial/sertest.asm",
    ),
    (
        "crates/gp64-machine/tests/serial/caller-ath.com",
        "crates/gp64-machine/tests/serial/sertest.asm",
    ),
    (
        "crates/gp64-machine/tests/serial/caller.com",
        "crates/gp64-machine/tests/serial/sertest.asm",
    ),
];
// Upstream test comment contained a stray NUL; replaced with an escape; historic blobs reviewed.
const HISTORIC_BINARY_BLOBS: &[&str] = &[
    "7890c1c84e692a4858ba514018b6525881bbb093",
    "b4ee426346204b528bb3eb817651050f3b4fd4c3",
];
// Reviewed 16-byte collisions, grouped by OUR source blob and exact offsets.
// Blob IDs bind each decision to the reviewed content, including old history.
// The strings/bytes from the owned image are deliberately not recorded.
const COLLISIONS: &[(&str, &str, &[usize], &str)] = &[
    (
        "crates/gp64-dos/src/dpmi/mod.rs",
        "88658ac3b421e9773369be191b2f4c7a9f82fc49",
        &[2435, 42170, 45410],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/filesystem.rs",
        "9681e1988f642df4fd4200cce138809bd35a537c",
        &[79752],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/int21/mod.rs",
        "799381f7f7a3e57a33b9beaeab695b19ab1c4fc2",
        &[340986, 340987, 340988],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/sfn.rs",
        "e9a0bd7a2ade2e11a6843e99b526dcab13825c66",
        &[
            27812, 27813, 27814, 27815, 27816, 27817, 27818, 27819, 27820, 27821, 27822,
        ],
        "Generic ASCII alphabet test string; reviewed.",
    ),
    (
        "crates/gp64-interp/src/lib.rs",
        "69f3231b870e8d6c9205539a59d9202a9199153b",
        &[698580],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-interp/src/paging_integration_tests.rs",
        "ca5f004e2907f1fb24f688881dfec919815aebc7",
        &[
            50249, 50250, 50251, 50252, 50253, 50254, 50255, 50256, 50257, 50258, 50259,
        ],
        "Generic ASCII alphabet test string; reviewed.",
    ),
    (
        "crates/gp64-memory/src/direct_proof.rs",
        "8c695d3197d6a183eb62a92f39743778504a51fc",
        &[21007],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "LICENSE",
        "f288702d2fa16d3cdf0035b15a9fcbc552cd88e7",
        &[6345, 6346],
        "Verbatim FSF GPL text, checked against the supplied source and FSF.",
    ),
    (
        "README.md",
        "f54b8f4aef843cef4c2b04460cd45d090054622f",
        &[3398, 3399, 3400, 3401, 3402, 3403, 3404],
        "Reviewed non-affiliation notice; permitted product-name text.",
    ),
    (
        "crates/gp64-bios/src/tests/bios_memory_and_startup.rs",
        "5b39fc2dc537bef2e6279f80aace995d230b7c95",
        &[9364],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-bios/src/tests/bios_memory_and_startup.rs",
        "acdaad9f83951aa14297415de79c563905c7797e",
        &[9379],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/dpmi/interrupts.rs",
        "3d817c68439a56425686ac29ffc03014e451c4ab",
        &[7349],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/dpmi/interrupts.rs",
        "f304146aa1a5bcfac09380aed44e6f4d4e31d090",
        &[6853],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/dpmi/mod.rs",
        "038c607e511133afe66e469014d46dbd565eaa68",
        &[2435, 42286, 45525],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/dpmi/mod.rs",
        "b495c6b888fdcd7d81108007a6d7feceacb3ff50",
        &[2435, 42300, 45539],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/filesystem.rs",
        "35fbc4c591b4a082b9d26c2cc8dd38b1aba38fde",
        &[79734],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/filesystem.rs",
        "45a64937558a8e79d9d93f3566051982d1097e78",
        &[130426],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/filesystem.rs",
        "ca89f0751d79159972ebbabc531b060aece20ea9",
        &[79743],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/int21/mod.rs",
        "0936248e8e72d5f15a7b0e3dea24a174934b9b30",
        &[345137, 345138, 345139],
        "Generic DOS program-termination diagnostic and its regression checks.",
    ),
    (
        "crates/gp64-dos/src/int21/mod.rs",
        "58226449263407f87edaedba9dce826e9843df6a",
        &[344691, 344692, 344693],
        "Generic DOS program-termination diagnostic and its regression checks.",
    ),
    (
        "crates/gp64-dos/src/int21/mod.rs",
        "65488bbfdc9aba6dc436889677f460ed5aad3bdc",
        &[344708, 344709, 344710],
        "Generic DOS program-termination diagnostic and its regression checks.",
    ),
    (
        "crates/gp64-dos/src/sfn.rs",
        "64dd89e74a45c1a8985090bba1166e112a77594e",
        &[
            27800, 27801, 27802, 27803, 27804, 27805, 27806, 27807, 27808, 27809, 27810,
        ],
        "Independent DOS short-filename test enumerates ASCII capital letters.",
    ),
    (
        "crates/gp64-dos/src/sfn.rs",
        "f068281fe99bf90f7846fad937c7154523e6f4ce",
        &[
            27862, 27863, 27864, 27865, 27866, 27867, 27868, 27869, 27870, 27871, 27872,
        ],
        "Independent DOS short-filename test enumerates ASCII capital letters.",
    ),
    (
        "crates/gp64-dos/src/tests/aux_prn_devices.rs",
        "980920f61f84c05efaddcf5be485777720d188e9",
        &[1227],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-dos/src/tests/int21_process_and_time.rs",
        "6f3fd36cc690c29aa0c5c9adadf3d8c02f8c199e",
        &[65402, 65403, 65404, 66338, 66339, 66340],
        "Generic DOS program-termination diagnostic and its regression checks.",
    ),
    (
        "crates/gp64-dos/src/tests/int21_process_and_time.rs",
        "f384c296ee499652abcdca0db235010e4814b13c",
        &[65417, 65418, 65419, 66353, 66354, 66355],
        "Generic DOS program-termination diagnostic and its regression checks.",
    ),
    (
        "crates/gp64-dos/src/unpacker.rs",
        "5b9895e4d53fda35125d63fefba5f9d69ec6e16b",
        &[
            368, 369, 370, 371, 372, 373, 374, 20967, 20968, 20969, 20970, 20971, 20972, 20973,
        ],
        "Generic EXEPACK format diagnostic in the unpacker constant and comments.",
    ),
    (
        "crates/gp64-dos/src/unpacker.rs",
        "85ac75978d3cf94a9ecaa80837143e703a8e6d34",
        &[
            368, 369, 370, 371, 372, 373, 374, 20901, 20902, 20903, 20904, 20905, 20906, 20907,
        ],
        "Generic EXEPACK format diagnostic in the unpacker constant and comments.",
    ),
    (
        "crates/gp64-dos/src/unpacker.rs",
        "85c324fe4940b0683f84ce6abb7d1de8d11f4e1d",
        &[
            368, 369, 370, 371, 372, 373, 374, 20903, 20904, 20905, 20906, 20907, 20908, 20909,
        ],
        "Generic EXEPACK format diagnostic in the unpacker constant and comments.",
    ),
    (
        "crates/gp64-hw/src/serial/mod.rs",
        "32205ddcc11b2669e81740e3a301113937008162",
        &[102226],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-hw/src/serial/mod.rs",
        "c0128016d3f05aa9a615b2fe650bad3bf5d6e2a1",
        &[102213],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-interp/src/lib.rs",
        "2be85e88ce4946183ad6e17e1cb0accffca56e14",
        &[717016],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-interp/src/lib.rs",
        "500b851aeec8f65751c77e78b972c3fc6ed2d8d2",
        &[717016],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-interp/src/lib.rs",
        "cba6188b702e36e1b5c4ad426670490448a88f5b",
        &[717016],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-machine/tests/dosrun.rs",
        "2205f29e2169f2d81cb9429ff12480ebb9956a82",
        &[
            8408, 8409, 8410, 8411, 8412, 8413, 8414, 8687, 8688, 8689, 8690, 8691, 8692, 8693,
        ],
        "Independent synthetic EXEPACK fixture uses the generic packer diagnostic.",
    ),
    (
        "crates/gp64-machine/tests/dosrun.rs",
        "b642026bb803365f0dd0ad9b14e881f2de966da1",
        &[
            8595, 8596, 8597, 8598, 8599, 8600, 8601, 8874, 8875, 8876, 8877, 8878, 8879, 8880,
        ],
        "Independent synthetic EXEPACK fixture uses the generic packer diagnostic.",
    ),
    (
        "crates/gp64-memory/src/direct_proof.rs",
        "7de1a651d367a172b7dab19b547e7275cae6445f",
        &[21007],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-modem/src/qam.rs",
        "1ecbfd6a322df4d8f23e3efadd70dcb8d44fc7fd",
        &[53530, 55728],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-modem/src/qam.rs",
        "ef22cefc5ed066f6a972bca07921a212ce1c4128",
        &[53518, 55716],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-modem/src/v34_training.rs",
        "54b1d3c10c6bcc0e73a08983a839b758a2be2226",
        &[160132],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
    (
        "crates/gp64-modem/src/v34_training.rs",
        "f7d8dba53efc95a782c779d8eeac345babd06fb3",
        &[160120],
        "Generic hardware/DOS documentation prose; reviewed against the source context.",
    ),
];
// Suppress padding and repeated-pattern windows in the owned images, while
// keeping short, diverse code/data runs. Owned inputs never become fixtures.
const RUN: usize = 16;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}
fn git_at(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn working_files() -> Vec<(String, Vec<u8>)> {
    working_files_at(&root())
}
fn working_files_at(repo: &Path) -> Vec<(String, Vec<u8>)> {
    // Scan what will be committed without staging it. Ignored local scratch and
    // build output remain excluded; all tracked files are always included.
    let mut paths = BTreeSet::new();
    for args in [
        vec!["ls-files", "-z"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        for path in git_at(repo, &args)
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
        {
            paths.insert(String::from_utf8(path.to_vec()).expect("UTF-8 path"));
        }
    }
    paths
        .into_iter()
        .map(|path| {
            let data = fs::read(repo.join(&path)).unwrap_or_else(|e| panic!("read {path}: {e}"));
            (path, data)
        })
        .collect()
}
// These are the full bodies from `git log --all --format=%B`, NUL-framed
// with the commit ID so an exception cannot accidentally allow another commit.
fn commit_messages(visit: impl FnMut(&str, &[u8])) {
    commit_messages_at(&root(), visit);
}
fn commit_messages_at(repo: &Path, mut visit: impl FnMut(&str, &[u8])) {
    let log = git_at(repo, &["log", "--all", "--format=%H%x00%B%x00"]);
    let mut parts = log.split(|b| *b == 0);
    while let Some(id) = parts.next() {
        let id = std::str::from_utf8(id).unwrap().trim();
        if id.is_empty() {
            continue;
        }
        let message = parts.next().expect("commit message framing");
        visit(id, message);
    }
}
// Object names from rev-list are hints, not all historical paths. Read each
// reachable object once, then visit exact path/blob pairs from the cached trees.
fn history(visit: impl FnMut(&str, &str, &[u8])) {
    history_at(&root(), visit);
}
fn history_at(repo: &Path, mut visit: impl FnMut(&str, &str, &[u8])) {
    let objects = String::from_utf8(git_at(
        repo,
        &["rev-list", "--all", "--objects", "--no-object-names"],
    ))
    .unwrap();
    let refs =
        String::from_utf8(git_at(repo, &["for-each-ref", "--format=%(objectname)"])).unwrap();
    let mut roots: BTreeSet<String> = refs.lines().map(str::to_owned).collect();
    let mut blobs = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut child = Command::new("git")
        .arg("cat-file")
        .arg("--batch")
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("cat-file");
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    for oid in objects.lines().collect::<BTreeSet<_>>() {
        writeln!(input, "{oid}").unwrap();
        input.flush().unwrap();
        let mut header = String::new();
        output.read_line(&mut header).unwrap();
        let parts: Vec<_> = header.split_whitespace().collect();
        assert_eq!(parts.len(), 3, "unexpected cat-file header");
        let size: usize = parts[2].parse().unwrap();
        let mut data = vec![0; size];
        output.read_exact(&mut data).unwrap();
        let mut newline = [0];
        output.read_exact(&mut newline).unwrap();
        assert_eq!(newline, [b'\n']);
        match parts[1] {
            "blob" => {
                blobs.insert(oid.to_owned(), data);
            }
            "tree" => {
                trees.insert(oid.to_owned(), history_tree_entries(&data, oid.len() / 2));
            }
            "commit" | "tag" => {
                // The first header names the root tree or the tagged object.
                // Following all reachable tag targets also handles nested tags.
                let first = data.split(|byte| *byte == b'\n').next().unwrap();
                let (_, target) = std::str::from_utf8(first).unwrap().split_once(' ').unwrap();
                roots.insert(target.to_owned());
            }
            kind => panic!("unexpected history object type {kind}"),
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());

    // A tree reused under another directory needs another visit; an unchanged
    // tree at the same prefix does not. Deduplicate only exact path/blob pairs.
    let mut pending: Vec<_> = roots
        .into_iter()
        .filter(|oid| trees.contains_key(oid) || blobs.contains_key(oid))
        .map(|oid| (Vec::new(), oid))
        .collect();
    let mut visited_trees = BTreeSet::new();
    let mut pairs = BTreeSet::new();
    while let Some((path, oid)) = pending.pop() {
        if let Some(entries) = trees.get(&oid) {
            if !visited_trees.insert((path.clone(), oid)) {
                continue;
            }
            for (name, target) in entries {
                let mut child_path = path.clone();
                if !child_path.is_empty() {
                    child_path.push(b'/');
                }
                child_path.extend(name);
                pending.push((child_path, target.clone()));
            }
        } else {
            assert!(
                blobs.contains_key(&oid),
                "missing historical tree/blob {oid}"
            );
            pairs.insert((path, oid));
        }
    }
    // Preserve coverage of any reachable blob without a tree pathname.
    let named: BTreeSet<_> = pairs.iter().map(|(_, oid)| oid.clone()).collect();
    for oid in blobs.keys().filter(|oid| !named.contains(*oid)) {
        pairs.insert((Vec::new(), oid.clone()));
    }
    for (path, oid) in pairs {
        let path = std::str::from_utf8(&path).expect("UTF-8 historical path");
        visit(path, &oid, &blobs[&oid]);
    }
}

fn history_tree_entries(mut data: &[u8], oid_bytes: usize) -> Vec<(Vec<u8>, String)> {
    // Raw tree entries are mode SP name NUL binary-object-ID. Unlike textual
    // object-name hints, this framing preserves tabs, newlines and backslashes.
    let mut entries = Vec::new();
    while !data.is_empty() {
        let space = data
            .iter()
            .position(|byte| *byte == b' ')
            .expect("tree mode framing");
        let nul = data
            .iter()
            .position(|byte| *byte == 0)
            .expect("tree name framing");
        let oid_end = nul + 1 + oid_bytes;
        let oid = data.get(nul + 1..oid_end).expect("tree object ID framing");
        // Gitlinks name another repository's commit, not a blob in this one.
        if &data[..space] != b"160000" {
            entries.push((
                data[space + 1..nul].to_vec(),
                oid.iter().map(|byte| format!("{byte:02x}")).collect(),
            ));
        }
        data = &data[oid_end..];
    }
    entries
}
fn word_hits(text: &str, words: &[&str]) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let lower = line.to_ascii_lowercase();
            words.iter().any(|word| lower.contains(word)).then_some(i)
        })
        .collect()
}
fn scan_words(path: &str, data: &[u8], label: &str, hits: &mut BTreeSet<String>) {
    if PLANNING.contains(&path) {
        return;
    }
    if let Ok(text) = std::str::from_utf8(data) {
        let lines: Vec<_> = text.lines().collect();
        for i in word_hits(text, WORDS) {
            let entry = format!("{path}:{}", lines[i]);
            if !WORD_ALLOWLIST.contains(&entry.as_str())
                && !COMMIT_WORD_ALLOWLIST.contains(&entry.as_str())
            {
                hits.insert(format!("{label}:{}", i + 1));
            }
        }
    }
}
fn binary_ok(path: &str, data: &[u8]) -> bool {
    (std::str::from_utf8(data).is_ok()
        && !data
            .iter()
            .any(|byte| *byte < 32 && !byte.is_ascii_whitespace()))
        || BINARY_ALLOWLIST.iter().any(|(allowed, _)| *allowed == path)
}
fn assert_clean(name: &str, hits: &BTreeSet<String>) {
    assert!(
        hits.is_empty(),
        "{name}: {} review candidates:\n{}",
        hits.len(),
        hits.iter().cloned().collect::<Vec<_>>().join("\n")
    );
}
#[test]
fn words_in_tracked_text_history_and_commit_messages() {
    let mut hits = BTreeSet::new();
    for (path, data) in working_files() {
        scan_words(&path, &data, &path, &mut hits);
    }
    history(|path, oid, data| scan_words(path, data, &format!("history {oid} {path}"), &mut hits));
    commit_messages(|id, message| {
        let label = format!("commit/{id}");
        scan_words(&label, message, &label, &mut hits);
    });
    assert_clean("words", &hits);
}
#[test]
fn binaries_have_reviewed_sources() {
    let files = working_files();
    for (_, source) in BINARY_ALLOWLIST {
        assert!(
            files.iter().any(|(path, _)| path == source),
            "missing binary source: {source}"
        );
    }
    let mut hits = BTreeSet::new();
    for (path, data) in files {
        if !binary_ok(&path, &data) {
            hits.insert(path);
        }
    }
    history(|path, oid, data| {
        if !binary_ok(path, data) && !HISTORIC_BINARY_BLOBS.contains(&oid) {
            hits.insert(format!("history {oid} {path}"));
        }
    });
    assert_clean("binaries", &hits);
}
// A private value is a FULL segment:offset pair. Match only whole tokens,
// with optional hex prefixes/suffixes and one extra leading zero on the offset.
type Address = (u16, u16);
fn address_part(token: &[u8], offset: bool) -> Option<u16> {
    let token = if token.len() > 2 && token[0] == b'0' && token[1].eq_ignore_ascii_case(&b'x') {
        &token[2..]
    } else {
        token
    };
    let token = if token.last().is_some_and(|b| b.eq_ignore_ascii_case(&b'h')) {
        &token[..token.len() - 1]
    } else {
        token
    };
    let token = if offset && token.len() == 5 && token[0] == b'0' {
        &token[1..]
    } else {
        token
    };
    if token.len() != 4 || !token.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    u16::from_str_radix(std::str::from_utf8(token).unwrap(), 16).ok()
}
fn address_pairs(data: &[u8]) -> Vec<(usize, Address)> {
    let mut pairs = Vec::new();
    let mut start = 0;
    while start < data.len() {
        if !data[start].is_ascii_alphanumeric() && data[start] != b'_' {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < data.len() && (data[end].is_ascii_alphanumeric() || data[end] == b'_') {
            end += 1;
        }
        if let Some(segment) = address_part(&data[start..end], false) {
            let mut colon = end;
            while colon < data.len() && data[colon].is_ascii_whitespace() {
                colon += 1;
            }
            if data.get(colon) == Some(&b':') {
                let mut right = colon + 1;
                while right < data.len() && data[right].is_ascii_whitespace() {
                    right += 1;
                }
                let mut finish = right;
                while finish < data.len()
                    && (data[finish].is_ascii_alphanumeric() || data[finish] == b'_')
                {
                    finish += 1;
                }
                if let Some(offset) = address_part(&data[right..finish], true) {
                    pairs.push((start, (segment, offset)));
                }
            }
        }
        start = end;
    }
    pairs
}
fn audit_hits(data: &[u8], denied: &HashSet<Address>) -> Vec<usize> {
    address_pairs(data)
        .into_iter()
        .filter_map(|(offset, pair)| denied.contains(&pair).then_some(offset))
        .collect()
}
fn audit_list(path: &Path) -> HashSet<Address> {
    let text = fs::read_to_string(path).expect("read private audit list");
    let mut denied = HashSet::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((left, right)) = line.split_once(':') else {
            panic!("invalid audit pair at line {}", i + 1);
        };
        assert!(
            left.len() == 4 && right.len() == 4,
            "audit list needs full four-digit pairs at line {}",
            i + 1
        );
        let segment = address_part(left.as_bytes(), false)
            .unwrap_or_else(|| panic!("invalid audit segment at line {}", i + 1));
        let offset = address_part(right.as_bytes(), true)
            .unwrap_or_else(|| panic!("invalid audit offset at line {}", i + 1));
        denied.insert((segment, offset));
    }
    assert!(!denied.is_empty(), "empty private audit list");
    denied
}
#[test]
#[ignore = "owned: set GP64_AUDIT_LIST and run with --ignored"]
fn private_audit_tracked_history_and_commit_messages() {
    let Some(path) = env::var_os("GP64_AUDIT_LIST") else {
        eprintln!("GP64_AUDIT_LIST unset; skipped");
        return;
    };
    let denied = audit_list(&root().join(path));
    let mut hits = BTreeSet::new();
    let mut matches = 0;
    let mut scan = |label: &str, data: &[u8]| {
        for offset in audit_hits(data, &denied) {
            matches += 1;
            let line = 1 + data[..offset].iter().filter(|byte| **byte == b'\n').count();
            hits.insert(format!("{label}:{line}"));
        }
    };
    for (path, data) in working_files() {
        scan(&path, &data);
    }
    history(|path, oid, data| scan(&format!("history {oid} {path}"), data));
    commit_messages(|id, message| scan(&format!("commit/{id}"), message));
    eprintln!(
        "private audit: {} full pairs; {matches} matches; {} matching file:lines",
        denied.len(),
        hits.len()
    );
    assert_clean("private audit", &hits);
}
fn diverse_window(window: &[u8]) -> bool {
    let mut counts = [0_usize; 256];
    let mut distinct = 0;
    for byte in window {
        let count = &mut counts[usize::from(*byte)];
        if *count == 0 {
            distinct += 1;
        }
        *count += 1;
        if *count > window.len() / 2 {
            return false;
        }
    }
    distinct >= 8
}
fn source_blob_id(repo: &Path, data: &[u8]) -> String {
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("hash reviewed source");
    child.stdin.take().unwrap().write_all(data).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "hash-object failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
// Decode separate byte tokens without joining through arbitrary source text.
// Preserve each token's source offset for candidate diagnostics and review.
fn encoded_runs(data: &[u8], prefix: &[u8], comma: bool) -> Vec<Vec<(usize, u8)>> {
    let mut runs = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        if prefix != b"\\x"
            && offset > 0
            && (data[offset - 1].is_ascii_alphanumeric() || data[offset - 1] == b'_')
        {
            offset += 1;
            continue;
        }
        let mut run = Vec::new();
        let mut cursor = offset;
        loop {
            let token = cursor;
            if !data
                .get(cursor..cursor + prefix.len())
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
            {
                break;
            }
            cursor += prefix.len();
            let Some(pair) = data.get(cursor..cursor + 2) else {
                break;
            };
            if !pair.iter().all(u8::is_ascii_hexdigit)
                || (prefix != b"\\x"
                    && data
                        .get(cursor + 2)
                        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_'))
            {
                break;
            }
            let value = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
            run.push((token, value));
            cursor += 2;
            let separator = cursor;
            while data.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if comma && data.get(cursor) == Some(&b',') {
                cursor += 1;
                while data.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
            }
            if prefix != b"\\x" && cursor == separator {
                break;
            }
        }
        offset = if let Some((last, _)) = run.last() {
            *last + prefix.len() + 2
        } else {
            offset + 1
        };
        if !run.is_empty() {
            runs.push(run);
        }
    }
    runs
}
struct ImageRuns {
    runs: HashSet<Vec<u8>>,
    length: usize,
}
impl ImageRuns {
    fn new(images: &[&[u8]], length: usize) -> Self {
        assert!(length >= 12);
        let runs = images
            .iter()
            .flat_map(|image| {
                image
                    .windows(length)
                    .filter(|window| diverse_window(window))
                    .map(<[u8]>::to_vec)
            })
            .collect();
        Self { runs, length }
    }
    fn matches(&self, data: &[u8]) -> Vec<(usize, &'static str)> {
        let mut hits = Vec::new();
        for (offset, run) in data.windows(self.length).enumerate() {
            if self.runs.contains(run) {
                hits.push((offset, "raw"));
            }
        }
        // Contiguous hex only. Both nibble alignments, both cases, byte offsets
        // in OUR file. No token boundary restriction: catches embedded hex.
        let mut start = 0;
        while start < data.len() {
            if !data[start].is_ascii_hexdigit() {
                start += 1;
                continue;
            }
            let mut end = start;
            while end < data.len() && data[end].is_ascii_hexdigit() {
                end += 1;
            }
            for parity in 0..2 {
                let decoded: Vec<_> = data[(start + parity).min(end)..end]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| {
                        let nibble = |b: u8| {
                            if b.is_ascii_digit() {
                                b - b'0'
                            } else {
                                b.to_ascii_lowercase() - b'a' + 10
                            }
                        };
                        nibble(pair[0]) * 16 + nibble(pair[1])
                    })
                    .collect();
                for (i, run) in decoded.windows(self.length).enumerate() {
                    if self.runs.contains(run) {
                        hits.push((start + parity + 2 * i, "hex"));
                    }
                }
            }
            start = end;
        }
        for (prefix, comma, kind) in [
            (b"0x".as_slice(), true, "byte array"),
            (b"".as_slice(), false, "spaced hex"),
            (b"\\x".as_slice(), false, "escaped hex"),
        ] {
            for tokens in encoded_runs(data, prefix, comma) {
                let decoded: Vec<_> = tokens.iter().map(|(_, byte)| *byte).collect();
                for (i, run) in decoded.windows(self.length).enumerate() {
                    if self.runs.contains(run) {
                        hits.push((tokens[i].0, kind));
                    }
                }
            }
        }
        hits
    }
}
#[test]
#[ignore = "owned: set GP64_GAME_DIR and run with --ignored"]
fn captured_bytes_tracked_and_history() {
    let Some(folder) = env::var_os("GP64_GAME_DIR") else {
        eprintln!("GP64_GAME_DIR unset; skipped");
        return;
    };
    let raw = fs::read(root().join(folder).join("GP.EXE")).expect("read owned executable");
    let unpacked = gp64_dos::unpacker::unpack_exepack(&raw).expect("EXEPACK unpack");
    assert_ne!(
        raw, unpacked,
        "unpacker returned raw image; unpacked audit would be missing"
    );
    let length = env::var("GP64_HYGIENE_RUN_LENGTH")
        .map_or(RUN, |value| value.parse().expect("invalid run length"));
    let matcher = ImageRuns::new(&[&raw, &unpacked], length);
    let (hits, reviewed) = captured_candidates(&root(), &matcher, length);
    eprintln!("raw + EXEPACK-unpacked: {length}-byte diverse runs; {reviewed} reviewed collisions; {} unreviewed candidates", hits.len());
    assert_clean("captured bytes", &hits);
}
fn captured_candidates(
    repo: &Path,
    matcher: &ImageRuns,
    length: usize,
) -> (BTreeSet<String>, usize) {
    let mut hits = BTreeSet::new();
    let mut reviewed = 0;
    let mut scan = |path: &str, label: &str, data: &[u8]| {
        let candidates = matcher.matches(data);
        let blob = if candidates.is_empty() {
            String::new()
        } else {
            source_blob_id(repo, data)
        };
        for (offset, _) in candidates {
            if length == RUN
                && COLLISIONS.iter().any(|(file, oid, offsets, reason)| {
                    *file == path && *oid == blob && offsets.contains(&offset) && !reason.is_empty()
                })
            {
                reviewed += 1;
            } else {
                let line = 1 + data[..offset].iter().filter(|byte| **byte == b'\n').count();
                hits.insert(format!("{label}:{line}"));
            }
        }
    };
    for (path, data) in working_files_at(repo) {
        scan(&path, &path, &data);
    }
    history_at(repo, |path, oid, data| {
        scan(path, &format!("history {oid} {path}"), data)
    });
    // Commit identities occupy a separate namespace from reviewed file blobs.
    // Reuse the same raw/encoded matcher and disclose only IDs and line numbers.
    commit_messages_at(repo, |id, data| {
        let label = format!("commit/{id}");
        scan(&label, &label, data);
    });
    (hits, reviewed)
}

#[test]
fn checker_self_test_generated_clean_and_contaminated() {
    // These generated fixtures contain NO real game material. Use synthetic
    // words, numeric values and image data, including binary and hex plantings.
    let words = ["synthetic contamination"];
    let clean = b"independently written hardware documentation";
    let dirty = [b"SYNTHETIC CONTAMINATION\n".as_slice(), &[0xff]].concat();
    assert!(word_hits(std::str::from_utf8(clean).unwrap(), &words).is_empty());
    assert_eq!(
        word_hits(
            std::str::from_utf8(&dirty[..dirty.len() - 1]).unwrap(),
            &words
        ),
        [0]
    );
    assert!(binary_ok("synthetic.txt", clean));
    assert!(!binary_ok("synthetic.bin", &dirty));
    let denied = HashSet::from([(0xd39b, 0x7a6c)]);
    assert!(audit_hits(clean, &denied).is_empty());
    for spelling in [
        "D39B:7A6C",
        "d39bh:7a6ch",
        "0xd39b:0x7a6c",
        "0xD39BH:0x07A6Ch",
        "d39b:07a6c",
        "D39BH : 0x7A6C",
    ] {
        assert_eq!(audit_hits(spelling.as_bytes(), &denied), [0]);
    }
    assert!(audit_hits(b"D39B 7A6C D39B:7A6D D39C:7A6C AD39B:7A6C D39B:7A6C0 D39B:007A6C xD39B:7A6C D39B:7A6C_suffix", &denied).is_empty());
    assert!(!diverse_window(&[0; RUN]));
    assert!(!diverse_window(&[0xff; RUN]));
    assert!(!diverse_window(&[b' '; RUN]));
    assert!(!diverse_window(&[1, 2, 3, 4].repeat(4)));
    // Exactly half one byte is accepted if there are at least eight values;
    // one more repeated byte is rejected even with sufficient diversity.
    assert!(diverse_window(&[vec![0; 8], (1..=8).collect()].concat()));
    assert!(!diverse_window(&[vec![0; 9], (1..=7).collect()].concat()));
    let padding = vec![b' '; RUN * 2];
    assert!(ImageRuns::new(&[&padding], RUN)
        .matches(&padding)
        .is_empty());
    let image: Vec<u8> = (0..128).map(|i| (i * 37 + 139) as u8).collect();
    for length in [12, RUN, 24] {
        let matcher = ImageRuns::new(&[&image], length);
        assert!(matcher.matches(clean).is_empty());
        let planted = [b"prefix".as_slice(), &image[7..7 + length], b"suffix"].concat();
        assert_eq!(matcher.matches(&planted), [(6, "raw")]);
        let hex: String = image[7..7 + length]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        assert_eq!(matcher.matches(hex.as_bytes()), [(0, "hex")]);
        assert_eq!(
            matcher.matches(hex.to_ascii_lowercase().as_bytes()),
            [(0, "hex")]
        );
        assert_eq!(matcher.matches(format!("a{hex}").as_bytes()), [(1, "hex")]);
        assert!(matcher.matches(&image[7..7 + length - 1]).is_empty());
    }
    // Prove exact exceptions do not accidentally widen to a whole file.
    let mut hits = BTreeSet::new();
    let line = WORD_ALLOWLIST[0].split_once(':').unwrap().1;
    scan_words(
        "crates/gp64-machine/src/launcher.rs",
        line.as_bytes(),
        "fixture",
        &mut hits,
    );
    assert!(hits.is_empty());
    scan_words(
        "crates/gp64-machine/src/launcher.rs",
        format!("{line} // extra").as_bytes(),
        "fixture",
        &mut hits,
    );
    assert_eq!(hits.len(), 1);
}

#[test]
fn launcher_commit_exception_is_exact() {
    let (path, line) = COMMIT_WORD_ALLOWLIST[0].split_once(':').unwrap();
    let mut hits = BTreeSet::new();
    scan_words(path, line.as_bytes(), "fixture", &mut hits);
    assert!(hits.is_empty());
    scan_words("commit/another", line.as_bytes(), "fixture", &mut hits);
    assert_eq!(hits.len(), 1);
    hits.clear();
    scan_words(
        path,
        format!("{line} changed").as_bytes(),
        "fixture",
        &mut hits,
    );
    assert_eq!(hits.len(), 1);
}

#[test]
fn generated_utf8_control_bytes_are_binary() {
    assert!(binary_ok("synthetic.txt", b"text\t\n\rtext"));
    for control in 0_u8..32 {
        if !control.is_ascii_whitespace() {
            let fixture = [b"text".as_slice(), &[control], b"text"].concat();
            assert!(!binary_ok("synthetic.bin", &fixture), "control {control}");
        }
    }
}

// An arithmetic permutation generates distinct bytes independently of any input.
fn synthetic_image() -> Vec<u8> {
    (0..RUN).map(|i| (i * 73 + 157) as u8).collect()
}

#[test]
fn generated_encoded_byte_arrays_are_caught() {
    let image = synthetic_image();
    assert!(diverse_window(&image));
    let matcher = ImageRuns::new(&[&image], RUN);
    for separator in [", ", ",", ",\n"] {
        let body = image
            .iter()
            .map(|b| format!("0x{b:02x}"))
            .collect::<Vec<_>>()
            .join(separator);
        assert_eq!(matcher.matches(body.as_bytes()), [(0, "byte array")]);
        assert_eq!(
            matcher.matches(body.to_ascii_uppercase().as_bytes()),
            [(0, "byte array")]
        );
        assert!(matcher
            .matches(body.replacen(separator, " + ", 1).as_bytes())
            .is_empty());
        assert_eq!(
            matcher.matches(format!("[{body}]").as_bytes()),
            [(1, "byte array")]
        );
    }
}

#[test]
fn generated_spaced_hex_bytes_are_caught() {
    let image = synthetic_image();
    let matcher = ImageRuns::new(&[&image], RUN);
    let body = image
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(matcher.matches(body.as_bytes()), [(0, "spaced hex")]);
    assert!(matcher
        .matches(body.replacen(' ', "word", 1).as_bytes())
        .is_empty());
}

#[test]
fn generated_escaped_hex_bytes_are_caught() {
    let image = synthetic_image();
    let matcher = ImageRuns::new(&[&image], RUN);
    let body: String = image.iter().map(|b| format!("\\x{b:02x}")).collect();
    assert_eq!(matcher.matches(body.as_bytes()), [(0, "escaped hex")]);
    assert_eq!(
        matcher.matches(format!("text{body}").as_bytes()),
        [(4, "escaped hex")]
    );
    assert!(matcher
        .matches(body.replacen("\\x", "\\q", 1).as_bytes())
        .is_empty());
}

#[test]
fn generated_escaped_hex_bytes_followed_by_ascii_text_are_caught() {
    let image = synthetic_image();
    let matcher = ImageRuns::new(&[&image], RUN);
    let body: String = image.iter().map(|b| format!("\\x{b:02x}")).collect();
    for suffix in ["word", "7", "_", "mix3d_text"] {
        let source = format!("b\"{body}{suffix}\"");
        assert_eq!(
            matcher.matches(source.as_bytes()),
            [(2, "escaped hex")],
            "suffix {suffix:?}"
        );
    }
}

#[test]
fn generated_numeric_hex_bytes_keep_ascii_token_boundaries() {
    let image = synthetic_image();
    let matcher = ImageRuns::new(&[&image], RUN);
    let byte_array = image
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect::<Vec<_>>()
        .join(", ");
    let spaced_hex = image
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    for encoding in [&byte_array, &spaced_hex] {
        for suffix in ["word", "7", "_", "mix3d_text"] {
            assert!(
                matcher
                    .matches(format!("{encoding}{suffix}").as_bytes())
                    .is_empty(),
                "encoding ending in {encoding:?} accepted suffix {suffix:?}"
            );
        }
    }
}

#[test]
fn generated_history_word_exemptions_keep_exact_paths() {
    let fixture = history_path_fixture("words", "sha1");
    let repo = &fixture.0;
    let (allowed, line) = WORD_ALLOWLIST[0].split_once(':').unwrap();
    let data = format!("{line}\n").into_bytes();
    seed_history_paths(repo, allowed, &data);
    let mut hits = BTreeSet::new();
    history_at(repo, |path, oid, bytes| {
        scan_words(path, bytes, &format!("history {oid} {path}"), &mut hits)
    });
    let oid = source_blob_id(repo, &data);
    let expected = historical_forbidden_paths()
        .into_iter()
        .map(|path| format!("history {oid} {path}:1"))
        .collect();
    assert_eq!(
        hits, expected,
        "deleted copies cannot inherit the original exemption"
    );
    git_at(repo, &["tag", "synthetic-tag", "synthetic-history"]);
    git_at(repo, &["branch", "-D", "synthetic-history"]);
    let mut tag_hits = BTreeSet::new();
    history_at(repo, |path, oid, bytes| {
        scan_words(path, bytes, &format!("history {oid} {path}"), &mut tag_hits)
    });
    assert_eq!(tag_hits, hits, "tag-only history keeps every exact path");
}

#[test]
fn generated_history_binary_and_captured_candidates_keep_exact_paths() {
    for format in ["sha1", "sha256"] {
        let fixture = history_path_fixture("bytes", format);
        let repo = &fixture.0;
        let allowed = BINARY_ALLOWLIST[0].0;
        let data = synthetic_image();
        seed_history_paths(repo, allowed, &data);
        let oid = source_blob_id(repo, &data);
        let mut paths = BTreeSet::new();
        let mut hits = BTreeSet::new();
        history_at(repo, |path, blob, bytes| {
            assert_eq!(blob, oid);
            assert_eq!(bytes, data);
            assert!(
                paths.insert(path.to_owned()),
                "duplicate exact path/blob pair"
            );
            if !binary_ok(path, bytes) {
                hits.insert(path.to_owned());
            }
        });
        let expected: BTreeSet<_> = historical_forbidden_paths()
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            hits, expected,
            "binary exemption is confined to its exact path"
        );
        assert_eq!(
            paths,
            expected
                .iter()
                .cloned()
                .chain([allowed.to_owned()])
                .collect()
        );
        let matcher = ImageRuns::new(&[&data], RUN);
        let (captured, reviewed) = captured_candidates(repo, &matcher, RUN);
        assert_eq!(reviewed, 0);
        assert_eq!(
            captured,
            paths
                .into_iter()
                .map(|path| format!("history {oid} {path}:1"))
                .chain([format!("{allowed}:1")])
                .collect()
        );
        // A subtree exposed by a nested annotated tag needs an additional root
        // context. A directly referenced blob needs its pathless context too.
        let tree =
            String::from_utf8(git_at(repo, &["rev-parse", "synthetic-history~1:nested"])).unwrap();
        git_at(
            repo,
            &[
                "-c",
                "user.name=Synthetic Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "tag.gpgsign=false",
                "tag",
                "-a",
                "synthetic-tree",
                "-m",
                "independent tree",
                tree.trim(),
            ],
        );
        git_at(
            repo,
            &[
                "-c",
                "user.name=Synthetic Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "tag.gpgsign=false",
                "tag",
                "-a",
                "synthetic-nested-tag",
                "-m",
                "independent nested tag",
                "synthetic-tree",
            ],
        );
        git_at(repo, &["update-ref", "refs/tags/synthetic-blob", &oid]);
        // Only the outer tag now refers to the standalone subtree.
        git_at(repo, &["tag", "-d", "synthetic-tree"]);
        let mut actual = BTreeSet::new();
        history_at(repo, |path, blob, _| {
            actual.insert((path.to_owned(), blob.to_owned()));
        });
        let mut expected_pairs: BTreeSet<_> = expected
            .into_iter()
            .chain([allowed.to_owned(), String::new()])
            .map(|path| (path, oid.clone()))
            .collect();
        for path in historical_forbidden_paths()
            .into_iter()
            .filter_map(|path| path.strip_prefix("nested/"))
        {
            expected_pairs.insert((path.to_owned(), oid.clone()));
        }
        assert_eq!(
            actual, expected_pairs,
            "standalone refs and shared trees keep distinct prefixes"
        );
    }
}

struct HistoryPathFixture(PathBuf);
impl Drop for HistoryPathFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove synthetic history repository");
    }
}

fn history_path_fixture(name: &str, format: &str) -> HistoryPathFixture {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let repo = root().join(".scratch").join(format!(
        "history-paths-{name}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&repo).unwrap();
    git_at(
        &repo,
        &["init", "--quiet", &format!("--object-format={format}")],
    );
    HistoryPathFixture(repo)
}

fn historical_forbidden_paths() -> [&'static str; 8] {
    [
        "copied.txt",
        "renamed.txt",
        "nested/tab\tname.txt",
        "nested/newline\nname.txt",
        "nested/space and\\backslash.txt",
        "shared/tab\tname.txt",
        "shared/newline\nname.txt",
        "shared/space and\\backslash.txt",
    ]
}

fn seed_history_paths(repo: &Path, allowed: &str, data: &[u8]) {
    let commit = |message| {
        git_at(
            repo,
            &[
                "-c",
                "user.name=Synthetic Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                message,
            ],
        );
    };
    fs::create_dir_all(repo.join(allowed).parent().unwrap()).unwrap();
    fs::write(repo.join(allowed), data).unwrap();
    git_at(repo, &["add", "--", allowed]);
    commit("independent allowed original");
    git_at(repo, &["branch", "-M", "synthetic-clean"]);
    git_at(repo, &["switch", "--quiet", "-c", "synthetic-history"]);
    let oid = source_blob_id(repo, data);
    let index = |mode: &str, path: &str| {
        // NUL-framed index plumbing creates historical names without requiring
        // the host filesystem to support their characters (notably Windows).
        let mut child = Command::new("git")
            .args(["update-index", "-z", "--index-info"])
            .current_dir(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{mode} {oid}\t{path}\0").as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "index fixture: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    for path in historical_forbidden_paths()
        .into_iter()
        .filter(|path| *path != "renamed.txt")
    {
        index("100644", path);
    }
    commit("independent copies");
    index("0", "copied.txt");
    index("100644", "renamed.txt");
    commit("independent renamed copy");
    for path in historical_forbidden_paths()
        .into_iter()
        .filter(|path| *path != "copied.txt")
    {
        index("0", path);
    }
    commit("independent removed copies");
    git_at(repo, &["switch", "--quiet", "synthetic-clean"]);
}

#[test]
fn generated_captured_content_in_reachable_commit_messages_is_caught() {
    // Only this test-created directory is removed, including on assertion failure.
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove synthetic repository");
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = root().join(".scratch");
    fs::create_dir_all(&scratch).unwrap();
    let repo = scratch.join(format!("captured-messages-{}-{nonce}", std::process::id()));
    fs::create_dir(&repo).unwrap();
    let fixture = Fixture(repo);
    let repo = &fixture.0;
    git_at(repo, &["init", "--quiet"]);
    fs::write(
        repo.join("clean.txt"),
        b"independent hardware documentation\n",
    )
    .unwrap();
    git_at(repo, &["add", "clean.txt"]);
    let commit = |body: &str| {
        git_at(
            repo,
            &[
                "-c",
                "user.name=Synthetic Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                body,
            ],
        );
        String::from_utf8(git_at(repo, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_owned()
    };
    commit("independent initial commit");
    git_at(repo, &["branch", "-M", "synthetic-clean"]);
    let raw: Vec<u8> = (0..RUN).map(|i| b'G' + i as u8).collect();
    let encoded = synthetic_image();
    let matcher = ImageRuns::new(&[&raw, &encoded], RUN);
    assert_eq!(
        captured_candidates(repo, &matcher, RUN),
        (BTreeSet::new(), 0)
    );
    git_at(repo, &["switch", "--quiet", "-c", "synthetic-messages"]);
    let mut expected = BTreeSet::new();
    let bodies = [
        String::from_utf8(raw).unwrap(),
        encoded
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        encoded
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" "),
        encoded
            .iter()
            .map(|b| format!("0x{b:02x}"))
            .collect::<Vec<_>>()
            .join(", "),
        encoded
            .iter()
            .map(|b| format!("\\x{b:02x}"))
            .collect::<String>(),
    ];
    for body in bodies {
        let id = commit(&format!("synthetic message\n\n{body}"));
        expected.insert(format!("commit/{id}:3"));
    }
    git_at(repo, &["tag", "synthetic-messages-tag"]);
    git_at(repo, &["switch", "--quiet", "synthetic-clean"]);
    let (hits, reviewed) = captured_candidates(repo, &matcher, RUN);
    assert_eq!(
        hits, expected,
        "message-only runs on a non-HEAD branch must be visited"
    );
    assert_eq!(
        reviewed, 0,
        "file collision reviews cannot suppress messages"
    );
    // The tag alone must also keep the message candidates reachable.
    git_at(repo, &["branch", "-D", "synthetic-messages"]);
    assert_eq!(captured_candidates(repo, &matcher, RUN), (expected, 0));
}
