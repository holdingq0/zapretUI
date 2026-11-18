// The portable program as one file. zapretUI-portable.exe is this app with the program attached to
// its end, the same signed pack the installer carries (rkn_core::release). Started, it puts the
// program into the folder "zapretUI" next to itself, unless the folder already holds this version or
// a newer one, and starts the window from there. The folder is an ordinary portable copy: its data,
// its updates and its autostart live in it, and the exe only brings it up to date.
//
// No signature check here: whoever could swap the pack could swap this code as well. The hashes
// catch a download that broke on the way; the service checks the engine against its own hashes
// before every start, and updates against the release key.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use rkn_core::ipc::{Message, Request};
use rkn_core::paths::{self, APP_EXE, PORTABLE_MARK, SERVICE_EXE};
use rkn_core::release::{self, FileEntry, Manifest, Place};
use windows::Win32::Foundation::{CloseHandle, LPARAM, WPARAM};
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, MB_ICONERROR, MB_OK, MessageBoxW, PostMessageW, WM_CLOSE};
use windows::core::{HSTRING, PCWSTR, w};

use crate::pipe;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The folder next to the exe the program lives in.
const FOLDER: &str = "zapretUI";
const STOP_WAIT: Duration = Duration::from_secs(15);

/// True when this exe is the one-file portable program: the copy in its folder has been started
/// (or the user was told why not), and this process has nothing more to do.
pub fn run() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    if !release::carries_pack(&exe) {
        return false;
    }
    if let Err(e) = launch(&exe) {
        unsafe {
            MessageBoxW(None, &HSTRING::from(format!("{e:#}")), w!("zapretUI"), MB_OK | MB_ICONERROR);
        }
    }
    true
}

fn launch(exe: &Path) -> anyhow::Result<()> {
    let root = exe.parent().context("папка программы")?.join(FOLDER);
    if stale(&root) {
        stop_running();
        unpack(exe, &root).with_context(|| {
            format!("Не удалось распаковать программу в {}. Переместите {} в папку, куда можно писать", root.display(), file_name(exe))
        })?;
    }
    std::process::Command::new(root.join(APP_EXE))
        .args(std::env::args_os().skip(1))
        .current_dir(&root)
        .spawn()
        .context("Не удалось запустить zapretUI")?;
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The copy in the folder is missing, incomplete or older than this exe. The marker holds the
/// version of the copy: this exe writes it, and the portable service rewrites it whenever it starts,
/// so a copy that has updated itself is not taken back to the version of an older download.
fn stale(root: &Path) -> bool {
    let version = std::fs::read_to_string(root.join(PORTABLE_MARK)).unwrap_or_default();
    let version = version.trim();
    let complete = [root.join(APP_EXE), root.join(SERVICE_EXE), root.join("winws2").join("winws2.exe")].iter().all(|p| p.is_file());
    !complete || version.is_empty() || paths::newer(VERSION, version)
}

fn window_alive() -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\zapretUI.app")) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

/// What answers on the pipe: Some(true) for a portable service, Some(false) for the installed one.
fn service() -> Option<bool> {
    match pipe::request(&Request::Status) {
        Ok(Message::Status(status)) => Some(status.portable),
        _ => None,
    }
}

/// A portable copy that runs holds its files: its window and its service are asked to quit, the
/// way "Выход" in the tray does, and are given some seconds for it. The installed service holds
/// nothing of the folder and keeps running.
fn stop_running() {
    let answering = service();
    if answering == Some(false) || (answering.is_none() && !window_alive()) {
        return;
    }
    unsafe {
        if let Ok(window) = FindWindowW(w!("zapretUI.tray"), PCWSTR::null()) {
            let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    if answering == Some(true) {
        let _ = pipe::request(&Request::Shutdown);
    }
    let deadline = Instant::now() + STOP_WAIT;
    while Instant::now() < deadline && (window_alive() || service() == Some(true)) {
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn sha256(bytes: &[u8]) -> String {
    release::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn target(root: &Path, entry: &FileEntry) -> anyhow::Result<PathBuf> {
    let safe = !entry.path.is_empty()
        && entry.path.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':']));
    ensure!(safe, "недопустимый путь: {}", entry.path);
    let base = match entry.place {
        Place::Bin => root.to_path_buf(),
        Place::Engine => root.join("winws2"),
    };
    Ok(entry.path.split('/').fold(base, |p, part| p.join(part)))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Writes next to the file and moves it in. A file that is still running can be moved aside but
/// not replaced: it goes to ".old", which the service removes once the new version has run a while.
fn put(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let fresh = with_suffix(path, ".new");
    std::fs::write(&fresh, data).with_context(|| format!("запись {}", path.display()))?;
    if std::fs::rename(&fresh, path).is_ok() {
        return Ok(());
    }
    let old = with_suffix(path, ".old");
    if old.exists() && std::fs::remove_file(&old).is_err() {
        let _ = std::fs::rename(&old, with_suffix(&old, &format!(".{}.bad", rkn_core::memory::now())));
    }
    let moved = std::fs::rename(path, &old).and_then(|_| std::fs::rename(&fresh, path));
    if moved.is_err() {
        let _ = std::fs::remove_file(&fresh);
    }
    moved.with_context(|| format!("{} занят", path.display()))
}

fn unpack(exe: &Path, root: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(exe)?;
    let broken = || format!("{} повреждён: скачайте его заново", file_name(exe));
    let (manifest_bytes, _, body) = release::attached(&bytes).and_then(release::unpack).with_context(broken)?;
    let manifest: Manifest = serde_json::from_slice(manifest_bytes).with_context(broken)?;
    ensure!(manifest.version == VERSION, "{}", broken());
    let parts = release::split(&manifest, body).with_context(broken)?;
    for (entry, data) in manifest.files.iter().zip(parts) {
        ensure!(sha256(data).eq_ignore_ascii_case(&entry.sha256), "{}", broken());
        let path = target(root, entry)?;
        if std::fs::read(&path).is_ok_and(|have| sha256(&have).eq_ignore_ascii_case(&entry.sha256)) {
            continue;
        }
        put(&path, data)?;
    }
    // Last: a folder whose unpacking broke off midway is unpacked again next time.
    paths::write_atomic(&root.join(PORTABLE_MARK), VERSION)?;
    Ok(())
}
