use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use rkn_core::paths;
use tracing_subscriber::EnvFilter;

const MAX_LOG: u64 = 2 * 1024 * 1024;

/// The log file, moved aside to ".old.log" once it grows past MAX_LOG: at start and while the
/// service runs, which may be months.
struct Rotating {
    path: PathBuf,
    file: Option<File>,
    size: u64,
}

impl Rotating {
    fn open(path: PathBuf) -> Self {
        let mut log = Self { path, file: None, size: 0 };
        log.reopen();
        log
    }

    fn reopen(&mut self) {
        // Closed first: Windows does not rename a file that is open.
        self.file = None;
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > MAX_LOG) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("old.log"));
        }
        self.file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).ok();
        self.size = self.file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
    }
}

impl Write for Rotating {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.size > MAX_LOG {
            self.reopen();
        }
        // A log that cannot be written must not stop the service.
        if let Some(file) = &mut self.file {
            if file.write_all(buf).is_ok() {
                self.size += buf.len() as u64;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn init(console: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if console {
        builder.init();
        return;
    }
    let path = paths::log_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    builder.with_ansi(false).with_writer(Mutex::new(Rotating::open(path))).init();
}
