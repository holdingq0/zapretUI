// Updates from GitHub Releases. The service checks the latest release now and then, downloads the
// files that changed, and installs them only when manifest.json carries a valid signature of the key
// built into this binary and every file matches its hash. Then a fresh copy of the service restarts it.

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, ensure};
use rkn_core::ipc::{Level, UpdateState};
use rkn_core::paths;
use rkn_core::release::{FileEntry, Manifest, Place, split, unhex, unpack, update_asset};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::hub::Hub;

const RELEASE: &str = include_str!("../../../resources/release.toml");
const FIRST_CHECK: Duration = Duration::from_secs(90);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const RETRY_BUSY: Duration = Duration::from_secs(20 * 60);
const KEEP_OLD: Duration = Duration::from_secs(120);
const MAX_UPDATE: usize = 128 * 1024 * 1024;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// What the user asked for. A check only looks; an install also downloads and restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Check,
    Install,
}

/// Left in the data folder right before the restart, so the new service can say what it became.
fn marker() -> PathBuf {
    paths::data_dir().join("updated.txt")
}

#[derive(Debug, Default, Deserialize)]
struct Source {
    repo: String,
    key: String,
}

fn source() -> Option<(String, Vec<u8>)> {
    let source: Source = toml::from_str(RELEASE).ok()?;
    let key = unhex(&source.key).filter(|k| k.len() == 32)?;
    let repo = source.repo.trim();
    (repo.split('/').count() == 2 && !repo.starts_with('/') && !repo.ends_with('/')).then(|| (repo.to_string(), key))
}

pub fn enabled() -> bool {
    source().is_some()
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

impl Release {
    fn version(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }

    fn asset(&self, name: &str) -> anyhow::Result<&Asset> {
        self.assets.iter().find(|a| a.name == name).with_context(|| format!("в релизе нет файла {name}"))
    }
}

#[derive(Debug)]
struct HttpStatus(u32);

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "сервер ответил {}", self.0)
    }
}

impl std::error::Error for HttpStatus {}

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// One HTTPS GET through WinHTTP: system proxy settings, redirects and TLS come from Windows.
fn get(url: &str, accept: &str, limit: usize) -> anyhow::Result<Vec<u8>> {
    let rest = url.strip_prefix("https://").context("адрес не https")?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    unsafe {
        let agent = HSTRING::from(format!("zapretUI/{}", env!("CARGO_PKG_VERSION")));
        let session = Handle(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        ensure!(!session.0.is_null(), "WinHTTP недоступен");
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 60_000)?;
        let connect = Handle(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0));
        ensure!(!connect.0.is_null(), "нет соединения с {host}");
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        ensure!(!request.0.is_null(), "запрос к {host} не создан");
        let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
        WinHttpAddRequestHeaders(request.0, &headers, WINHTTP_ADDREQ_FLAG_ADD)?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).with_context(|| format!("нет связи с {host}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).with_context(|| format!("{host} не ответил"))?;
        let mut code = 0u32;
        let mut size = 4u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut code as *mut u32).cast()),
            &mut size,
            std::ptr::null_mut(),
        )?;
        if code != 200 {
            return Err(HttpStatus(code).into());
        }
        let mut body: Vec<u8> = Vec::new();
        loop {
            let mut available = 0u32;
            WinHttpQueryDataAvailable(request.0, &mut available)?;
            if available == 0 {
                break;
            }
            let start = body.len();
            ensure!(start + available as usize <= limit, "файл больше {} МБ", limit / (1024 * 1024));
            body.resize(start + available as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(request.0, body[start..].as_mut_ptr().cast(), available, &mut read)?;
            body.truncate(start + read as usize);
        }
        Ok(body)
    }
}

fn sha256(bytes: &[u8]) -> String {
    rkn_core::release::hex(&Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| sha256(&b))
}

fn target(entry: &FileEntry) -> anyhow::Result<PathBuf> {
    let safe = !entry.path.is_empty()
        && entry.path.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':']));
    ensure!(safe, "недопустимый путь в манифесте: {}", entry.path);
    let base = match entry.place {
        Place::Bin => paths::install_dir(),
        Place::Engine => paths::engine_dir(),
    };
    Ok(entry.path.split('/').fold(base, |p, part| p.join(part)))
}

fn latest(repo: &str) -> anyhow::Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    match get(&url, "application/vnd.github+json", 1024 * 1024) {
        Ok(body) => {
            let release: Release = serde_json::from_slice(&body).context("ответ GitHub")?;
            Ok((!release.draft && !release.prerelease).then_some(release))
        }
        // No release published yet.
        Err(e) if e.downcast_ref::<HttpStatus>().is_some_and(|s| s.0 == 404) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Downloads and checks everything the release changes; returns the files to put in place.
fn download(release: &Release, key: &[u8]) -> anyhow::Result<Vec<(FileEntry, PathBuf)>> {
    let asset = release.asset(&update_asset(release.version()))?;
    let bytes = get(&asset.browser_download_url, "application/octet-stream", MAX_UPDATE)?;
    let (manifest_bytes, signature, body) = unpack(&bytes).context("файл обновления повреждён")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(manifest_bytes, signature)
        .map_err(|_| anyhow!("подпись обновления не совпадает, обновление отклонено"))?;
    let manifest: Manifest = serde_json::from_slice(manifest_bytes).context("список файлов обновления")?;
    ensure!(manifest.version == release.version(), "версия в обновлении не совпадает с релизом");
    ensure!(paths::newer(&manifest.version, env!("CARGO_PKG_VERSION")), "релиз не новее установленной версии");

    let dir = paths::update_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let mut out = Vec::new();
    let parts = split(&manifest, body).context("файл обновления обрезан")?;
    for (n, (entry, data)) in manifest.files.iter().zip(parts).enumerate() {
        ensure!(sha256(data).eq_ignore_ascii_case(&entry.sha256), "{} повреждён", entry.path);
        let to = target(entry)?;
        if sha256_file(&to).is_some_and(|h| h.eq_ignore_ascii_case(&entry.sha256)) {
            continue;
        }
        let file = dir.join(n.to_string());
        std::fs::write(&file, data)?;
        out.push((entry.clone(), file));
    }
    ensure!(!out.is_empty(), "в релизе нет изменённых файлов");
    Ok(out)
}

fn backup_of(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Running executables can be renamed but not overwritten: each file moves aside, the new one takes
/// its place. Any failure puts the old files back.
fn put_in_place(files: &[(FileEntry, PathBuf)]) -> anyhow::Result<()> {
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let result = (|| -> anyhow::Result<()> {
        for (entry, file) in files {
            let to = target(entry)?;
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let backup = to.exists().then(|| backup_of(&to));
            if let Some(backup) = &backup {
                let _ = std::fs::remove_file(backup);
                std::fs::rename(&to, backup).with_context(|| format!("{} занят", to.display()))?;
            }
            done.push((to.clone(), backup));
            // A copy, not a move: a moved file would keep the locked-down rights of the download folder
            // and users could not even start the new app. A copy takes the rights of where it lands.
            std::fs::copy(file, &to).with_context(|| format!("запись {}", to.display()))?;
        }
        Ok(())
    })();
    if result.is_err() {
        for (to, backup) in done.into_iter().rev() {
            let _ = std::fs::remove_file(&to);
            if let Some(backup) = backup {
                let _ = std::fs::rename(backup, &to);
            }
        }
    }
    result
}

/// Files an update left next to the real ones: "old" (moved aside) or "bad" (rolled back).
fn leftovers(ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in [paths::install_dir(), paths::engine_dir(), paths::engine_dir().join("lua")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        out.extend(entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == ext)));
    }
    out
}

/// Removes what the previous update moved aside, once the new version has proved itself.
/// The old app may still be running; then its file goes next time.
pub fn cleanup() {
    for old in leftovers("old").into_iter().chain(leftovers("bad")) {
        let _ = std::fs::remove_file(old);
    }
    let _ = std::fs::remove_dir_all(paths::update_dir());
}

/// A version that failed to start and was rolled back: never installed again on its own.
fn skipped() -> PathBuf {
    paths::data_dir().join("rolled-back.txt")
}

/// The new service did not start: every file moved aside goes back in place.
pub fn roll_back() {
    if let Ok(version) = std::fs::read_to_string(marker()) {
        let _ = std::fs::write(skipped(), version.trim());
    }
    for old in leftovers("old") {
        let current = old.with_extension("");
        let mut broken = current.as_os_str().to_owned();
        broken.push(".bad");
        let _ = std::fs::remove_file(&broken);
        // A running copy can be renamed away but not deleted.
        if current.exists() && std::fs::remove_file(&current).is_err() {
            let _ = std::fs::rename(&current, &broken);
        }
        let _ = std::fs::rename(&old, &current);
    }
    let _ = std::fs::remove_file(marker());
}

/// The installed service restarts through a helper that can roll back; a portable copy starts its
/// successor, which waits for this process to exit, and then quits.
fn restart_service() -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new(paths::service_exe());
    if paths::portable() {
        cmd.arg("portable").arg("--after").arg(std::process::id().to_string());
    } else {
        cmd.arg("restart");
    }
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).spawn().context("перезапуск службы")?;
    Ok(())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| anyhow!("{e}"))?
}

async fn check(hub: &Hub, repo: &str) -> Option<Release> {
    hub.set_update(UpdateState::Checking, None, None);
    let repo = repo.to_string();
    match blocking(move || latest(&repo)).await {
        Ok(Some(release)) if paths::newer(release.version(), env!("CARGO_PKG_VERSION")) => {
            hub.set_update(UpdateState::Available, Some(release.version().to_string()), None);
            Some(release)
        }
        Ok(_) => {
            hub.set_update(UpdateState::Latest, None, None);
            None
        }
        Err(e) => {
            tracing::warn!("update check: {e:#}");
            hub.set_update(UpdateState::Failed, None, Some(format!("Не удалось проверить обновления: {e:#}")));
            None
        }
    }
}

async fn install(hub: &Hub, release: Release, key: Vec<u8>) {
    let version = release.version().to_string();
    hub.set_update(UpdateState::Downloading, Some(version.clone()), None);
    let files = match blocking(move || download(&release, &key)).await {
        Ok(files) => files,
        Err(e) => {
            tracing::warn!("update download: {e:#}");
            hub.set_update(UpdateState::Failed, Some(version), Some(format!("{e:#}")));
            return;
        }
    };
    hub.set_update(UpdateState::Installing, Some(version.clone()), None);
    hub.stop_for_update();
    // Trial winws2 of a stopped search must be gone before their files are replaced and the driver goes.
    tokio::time::sleep(Duration::from_millis(300)).await;
    rkn_search::settle().await;
    tokio::task::spawn_blocking(crate::engine::unload_driver).await.ok();
    let done = version.clone();
    let applied = blocking(move || {
        put_in_place(&files)?;
        let _ = std::fs::write(marker(), &done);
        restart_service()
    })
    .await;
    match applied {
        Ok(()) => {
            hub.note(Level::Info, format!("Установлено обновление {version}. Перезапускаюсь"));
            if paths::portable() {
                hub.quit();
            }
        }
        Err(e) => {
            hub.resume_after_update();
            hub.set_update(UpdateState::Failed, Some(version), Some(format!("Обновление не установлено: {e:#}")));
        }
    }
}

pub async fn run(hub: Arc<Hub>, mut asks: mpsc::UnboundedReceiver<Ask>) {
    // Files of the previous version stay until this one has run for a while: they are the way back.
    tokio::spawn(async {
        tokio::time::sleep(KEEP_OLD).await;
        let _ = tokio::task::spawn_blocking(cleanup).await;
    });
    if let Ok(version) = std::fs::read_to_string(marker()) {
        let _ = std::fs::remove_file(marker());
        if version.trim() == env!("CARGO_PKG_VERSION") {
            // The app shows the notification itself when its new copy takes over.
            hub.note(Level::Info, format!("zapretUI обновлён до версии {}", version.trim()));
        }
    }
    let Some((repo, key)) = source() else {
        while asks.recv().await.is_some() {}
        return;
    };
    hub.set_update(UpdateState::Idle, None, None);
    let mut wait = FIRST_CHECK;
    let mut found: Option<Release> = None;
    loop {
        let ask = match tokio::time::timeout(wait, asks.recv()).await {
            Ok(Some(ask)) => Some(ask),
            Ok(None) => return,
            Err(_) => None,
        };
        wait = CHECK_EVERY;
        match ask {
            // The button in the window or the tray: install what was found, or look first.
            Some(Ask::Install) => {
                if found.is_none() {
                    found = check(&hub, &repo).await;
                }
                if let Some(release) = found.take() {
                    install(&hub, release, key.clone()).await;
                }
            }
            // A manual check only shows what is there; installing stays one more click away.
            Some(Ask::Check) => found = check(&hub, &repo).await,
            // The timer: with auto updates on, install, but never in the middle of a game or a search.
            None => {
                found = check(&hub, &repo).await;
                let Some(release) = found.take() else { continue };
                let rolled_back = std::fs::read_to_string(skipped()).is_ok_and(|v| v.trim() == release.version());
                if hub.auto_update() && !hub.busy() && !rolled_back {
                    install(&hub, release, key.clone()).await;
                    continue;
                }
                // Without auto updates the badge in the window and the tray menu offer it.
                if hub.auto_update() && !rolled_back {
                    wait = RETRY_BUSY;
                }
                found = Some(release);
            }
        }
    }
}
