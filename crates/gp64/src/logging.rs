//! The diagnostics log: `tracing` events, at `info` unless `RUST_LOG` says
//! otherwise, go to `gp64.log` in the per-user data folder. The file
//! flip-flops: once it passes [`MAX_BYTES`] it becomes `gp64.old.log` and a
//! fresh one starts, so the pair never holds much more than twice the cap.
//! A writer thread owns the file. Emulation never waits on the disk: when
//! the writer falls behind, records are dropped. Every line starts with the
//! process id, so two copies on one machine can share the file.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
};
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt, util::SubscriberInitExt};
use tracing_subscriber::{EnvFilter, Layer};

/// Size at which the log flips to its old copy.
const MAX_BYTES: u64 = 4 << 20;
/// Records waiting for the writer before new ones are dropped.
const QUEUE_RECORDS: usize = 4096;

/// `<data dir>/gp64/gp64.log`, where the platform has a data dir.
pub fn path() -> Option<PathBuf> {
    gp64_types::host_dirs::data_local_dir().map(|dir| dir.join("gp64").join("gp64.log"))
}

/// Starts logging for this run. A log that cannot open costs only the log:
/// one line on `stderr`, and the run goes on.
pub fn install() {
    let Some(path) = path() else {
        return;
    };
    let file = match FlipFlop::open(path.clone(), MAX_BYTES) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("gp64: no log at {}: {error}", path.display());
            return;
        }
    };
    let filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| EnvFilter::try_new(value).ok())
        .unwrap_or_else(|| EnvFilter::new("info"));
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(Records::spawn(file))
        .with_filter(filter);
    if tracing_subscriber::registry()
        .with(layer)
        .try_init()
        .is_ok()
    {
        tracing::info!(target: "gp64", "gp64 {} started", env!("CARGO_PKG_VERSION"));
    }
}

/// The log file and its old copy.
struct FlipFlop {
    path: PathBuf,
    old: PathBuf,
    file: Option<File>,
    /// Bytes in the file, as last known; another copy may also append.
    size: u64,
    max: u64,
}
impl FlipFlop {
    fn open(path: PathBuf, max: u64) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = Self::append(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            old: path.with_extension("old.log"),
            path,
            file: Some(file),
            size,
            max,
        })
    }
    fn append(path: &Path) -> io::Result<File> {
        OpenOptions::new().create(true).append(true).open(path)
    }
    fn write(&mut self, record: &[u8]) -> io::Result<()> {
        if self.size + record.len() as u64 > self.max {
            // Another copy's appends count too.
            if let Some(file) = &self.file {
                self.size = file.metadata()?.len();
            }
            if self.size > 0 && self.size + record.len() as u64 > self.max {
                self.flip()?;
            }
        }
        let file = match &mut self.file {
            Some(file) => file,
            None => self.file.insert(Self::append(&self.path)?),
        };
        file.write_all(record)?;
        self.size += record.len() as u64;
        Ok(())
    }
    /// The full log becomes the old copy, replacing the previous one.
    fn flip(&mut self) -> io::Result<()> {
        // Windows renames only a closed file of ours.
        self.file = None;
        match fs::rename(&self.path, &self.old) {
            Ok(()) => {}
            // Another copy flipped it first.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let file = Self::append(&self.path)?;
        self.size = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }
}

/// Hands each formatted record to the writer thread.
struct Records {
    sender: SyncSender<Vec<u8>>,
    prefix: String,
}
impl Records {
    fn spawn(file: FlipFlop) -> Self {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_RECORDS);
        let started = thread::Builder::new()
            .name("gp64-log".into())
            .spawn(move || drain(receiver, file));
        if let Err(error) = started {
            eprintln!("gp64: no log writer: {error}");
        }
        Self {
            sender,
            prefix: format!("[{}] ", std::process::id()),
        }
    }
}
fn drain(receiver: Receiver<Vec<u8>>, mut file: FlipFlop) {
    for record in receiver {
        // A full disk or a lost file costs only this record.
        let _ = file.write(&record);
    }
}
impl<'a> MakeWriter<'a> for Records {
    type Writer = Record;
    fn make_writer(&'a self) -> Record {
        let mut text = Vec::with_capacity(256);
        text.extend_from_slice(self.prefix.as_bytes());
        Record {
            sender: self.sender.clone(),
            text,
        }
    }
}
/// One record, sent whole when the formatter drops it.
struct Record {
    sender: SyncSender<Vec<u8>>,
    text: Vec<u8>,
}
impl Write for Record {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.text.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for Record {
    fn drop(&mut self) {
        let _ = self.sender.try_send(std::mem::take(&mut self.text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn folder(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("gp64 crate lives below the worktree root")
            .join(".scratch")
            .join(format!(
                "gp64-log-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&folder);
        folder
    }

    #[test]
    fn the_log_flips_to_its_old_copy_and_stays_bounded() {
        let folder = folder("flip");
        let path = folder.join("gp64.log");
        let mut log = FlipFlop::open(path.clone(), 100).unwrap();
        for index in 0..30 {
            log.write(format!("record {index:02} ........\n").as_bytes())
                .unwrap();
        }
        let current = fs::read_to_string(&path).unwrap();
        let old = fs::read_to_string(folder.join("gp64.old.log")).unwrap();
        assert!(
            current.len() <= 100 && old.len() <= 100,
            "{current:?} {old:?}"
        );
        // The newest record is current; the one before the flip is old.
        assert!(current.ends_with("record 29 ........\n"));
        let first_current: usize = current[7..9].parse().unwrap();
        assert!(old.ends_with(&format!("record {:02} ........\n", first_current - 1)));
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_new_run_appends_and_flips_when_the_file_is_already_full() {
        let folder = folder("reopen");
        let path = folder.join("gp64.log");
        FlipFlop::open(path.clone(), 100)
            .unwrap()
            .write(b"first run\n")
            .unwrap();
        let mut second = FlipFlop::open(path.clone(), 100).unwrap();
        second.write(b"second run\n").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "first run\nsecond run\n"
        );
        let mut full = FlipFlop::open(path.clone(), 20).unwrap();
        full.write(b"third run\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "third run\n");
        assert_eq!(
            fs::read_to_string(folder.join("gp64.old.log")).unwrap(),
            "first run\nsecond run\n"
        );
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn records_reach_the_file_whole_with_the_process_id() {
        let folder = folder("records");
        let path = folder.join("gp64.log");
        let records = Records::spawn(FlipFlop::open(path.clone(), MAX_BYTES).unwrap());
        for line in ["one\n", "two\n"] {
            let mut record = records.make_writer();
            record.write_all(line.as_bytes()).unwrap();
        }
        drop(records);
        let expected = format!("[{0}] one\n[{0}] two\n", std::process::id());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fs::read_to_string(&path).unwrap() != expected {
            assert!(std::time::Instant::now() < deadline, "log never completed");
            thread::sleep(std::time::Duration::from_millis(5));
        }
        fs::remove_dir_all(&folder).unwrap();
    }
}
