use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const PRODUCT: &str = "zapretUI";
pub const APP_EXE: &str = "zapretUI.exe";
pub const SERVICE_EXE: &str = "zapretUI-svc.exe";
/// A file with this name next to the exe makes the copy portable: everything lives in its folder.
pub const PORTABLE_MARK: &str = "zapretUI.portable";

/// The folder of a portable copy, or None for an installed one.
pub fn portable_root() -> Option<&'static Path> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        dir.join(PORTABLE_MARK).is_file().then(|| dir.to_path_buf())
    })
    .as_deref()
}

pub fn portable() -> bool {
    portable_root().is_some()
}

fn program_data() -> PathBuf {
    std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

pub fn data_dir() -> PathBuf {
    match portable_root() {
        Some(root) => root.join("data"),
        None => program_data().join(PRODUCT),
    }
}

/// Data of the builds that were still called rkn.
pub fn legacy_data_dir() -> PathBuf {
    program_data().join("rkn")
}

/// Program Files: users may read and run, only administrators and the service may write.
pub fn install_dir() -> PathBuf {
    match portable_root() {
        Some(root) => root.to_path_buf(),
        None => installed_dir(),
    }
}

/// Where the installer puts the program, even when asked from a portable copy.
pub fn installed_dir() -> PathBuf {
    std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
        .join(PRODUCT)
}

pub fn service_exe() -> PathBuf {
    install_dir().join(SERVICE_EXE)
}

pub fn app_exe() -> PathBuf {
    install_dir().join(APP_EXE)
}

/// The portable copy runs winws2 from the folder it came in; the installed one from its data folder.
pub fn engine_dir() -> PathBuf {
    match portable_root() {
        Some(root) => root.join("winws2"),
        None => data_dir().join("engine"),
    }
}

pub fn update_dir() -> PathBuf {
    data_dir().join("update")
}

pub fn state_dir() -> PathBuf {
    data_dir().join("state")
}

pub fn config_file() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn autohostlist_file() -> PathBuf {
    state_dir().join("autohostlist.txt")
}

pub fn always_file() -> PathBuf {
    state_dir().join("always.txt")
}

pub fn exclude_file() -> PathBuf {
    state_dir().join("exclude.txt")
}

pub fn sites_file() -> PathBuf {
    state_dir().join("sites.toml")
}

pub fn direct_file() -> PathBuf {
    state_dir().join("direct.txt")
}

pub fn probed_file() -> PathBuf {
    state_dir().join("probed.txt")
}

pub fn network_file(id: &str) -> PathBuf {
    state_dir().join("networks").join(format!("{id}.toml"))
}

pub fn log_file() -> PathBuf {
    data_dir().join("zapretUI-svc.log")
}

pub fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    // Flushed before the rename: a power cut must not leave an empty config or network memory.
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

/// Compares dotted versions like "0.2.10" and "0.2.9" numerically; a leading "v" is ignored.
pub fn newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.trim().trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(candidate) > parts(current)
}
