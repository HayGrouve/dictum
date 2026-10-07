//! Minimal file logger. Dictated text is never logged.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};

const MAX_SIZE: u64 = 2 * 1024 * 1024;

struct FileLogger {
    file: Mutex<File>,
    started: Instant,
    echo: bool,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Debug && metadata.target().starts_with("dictum")
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "[{:>9.3}] {:<5} {}\n",
            self.started.elapsed().as_secs_f64(),
            record.level(),
            record.args()
        );
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
        if self.echo {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

pub fn init(path: &Path) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_SIZE) {
        let _ = std::fs::rename(path, path.with_extension("old.log"));
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    writeln!(file, "\n=== Dictum {} started (unix time {unix}) ===", env!("CARGO_PKG_VERSION"))?;
    let logger = FileLogger { file: Mutex::new(file), started: Instant::now(), echo: cfg!(debug_assertions) };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(LevelFilter::Debug);
    }
    Ok(())
}
