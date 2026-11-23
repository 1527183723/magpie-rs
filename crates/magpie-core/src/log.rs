//! Process-wide logging: one append-only file plus a bounded in-memory ring that
//! both the built-in web UI and the Android log viewer read.
//!
//! Upstream magpie writes its log to a file the Android shell tails; we do the
//! same, and the ring buffer is what makes "show me the log" work without
//! re-reading the file from the UI thread.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const RING_CAPACITY: usize = 2000;
/// Rotate at 2 MiB, the same ceiling the Java shell used.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

struct Sink {
    file: Option<File>,
    path: Option<PathBuf>,
    ring: VecDeque<String>,
    echo: bool,
    level: Level,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();
static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();

fn sink() -> &'static Mutex<Sink> {
    SINK.get_or_init(|| {
        Mutex::new(Sink {
            file: None,
            path: None,
            ring: VecDeque::with_capacity(256),
            echo: false,
            level: Level::Info,
        })
    })
}

/// Point the log at `path` (created if missing) and mirror it to stderr.
///
/// Called once at startup. A second call rotates the destination, which is what
/// the Android shell does when the user clears the log.
pub fn init(path: Option<&Path>, echo: bool, level: Level) {
    EPOCH.get_or_init(std::time::Instant::now);
    let mut s = sink().lock().unwrap_or_else(|e| e.into_inner());
    s.echo = echo;
    s.level = level;
    s.path = path.map(|p| p.to_path_buf());
    s.file = None;
    if let Some(p) = path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(meta) = std::fs::metadata(p)
            && meta.len() > MAX_BYTES
        {
            let _ = std::fs::rename(p, p.with_extension("log.1"));
        }
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(p) {
            s.file = Some(f);
        }
    }
}

/// Truncate the log file and drop the in-memory ring.
pub fn clear() {
    let mut s = sink().lock().unwrap_or_else(|e| e.into_inner());
    s.ring.clear();
    if let Some(p) = s.path.clone() {
        let _ = std::fs::write(&p, b"");
        s.file = OpenOptions::new().create(true).append(true).open(p).ok();
    }
}

/// The last `n` lines, oldest first.
pub fn tail(n: usize) -> Vec<String> {
    let s = sink().lock().unwrap_or_else(|e| e.into_inner());
    let skip = s.ring.len().saturating_sub(n);
    s.ring.iter().skip(skip).cloned().collect()
}

/// Where the log lives, when it lives anywhere.
pub fn path() -> Option<PathBuf> {
    sink().lock().ok().and_then(|s| s.path.clone())
}

pub fn enabled(level: Level) -> bool {
    let s = sink().lock().unwrap_or_else(|e| e.into_inner());
    level <= s.level
}

/// Write one line. Prefer the `info!`/`warn!`/`error!` macros below.
pub fn emit(level: Level, args: std::fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    let tag = match level {
        Level::Error => "error",
        Level::Warn => "warn ",
        Level::Info => "info ",
        Level::Debug => "debug",
    };
    // Uptime rather than wall-clock: Android's clock is fine, but a monotonic
    // stamp keeps the ordering honest when the device changes time zone.
    let secs = EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs();
    let line = format!("[{:>6}.{:03}] {} {}", secs, 0, tag, args);

    let mut s = sink().lock().unwrap_or_else(|e| e.into_inner());
    if s.ring.len() == RING_CAPACITY {
        s.ring.pop_front();
    }
    s.ring.push_back(line.clone());
    if let Some(f) = s.file.as_mut() {
        let _ = writeln!(f, "{line}");
    }
    if s.echo {
        eprintln!("{line}");
    }
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::emit($crate::log::Level::Info, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::emit($crate::log::Level::Warn, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log::emit($crate::log::Level::Error, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log::emit($crate::log::Level::Debug, format_args!($($arg)*)) };
}
