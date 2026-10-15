use std::sync::Mutex;

use rkn_core::paths;
use tracing_subscriber::EnvFilter;

const MAX_LOG: u64 = 2 * 1024 * 1024;

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
    if std::fs::metadata(&path).map(|m| m.len() > MAX_LOG).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("old.log"));
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => builder.with_ansi(false).with_writer(Mutex::new(file)).init(),
        Err(_) => builder.with_ansi(false).with_writer(std::io::sink).init(),
    }
}
