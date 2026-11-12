// zapretUI-setup.exe: the program travels as a signed pack appended to this exe (see
// rkn_core::release). Windows asks for administrator rights on launch (manifest); the pack is
// checked against the release key, unpacked to a temp folder, `zapretUI-svc.exe install` puts the
// files in place, and the installed window opens with the user's own rights.
#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;

use anyhow::{Context, ensure};
use rkn_core::paths;
use rkn_core::release::{Manifest, Place, attached, hex, split, unhex, unpack};
use serde::Deserialize;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{BOOL, HSTRING, w};

const RELEASE: &str = include_str!("../../../resources/release.toml");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Deserialize)]
struct Source {
    key: String,
    /// Keys of earlier or coming releases that are accepted too (see resources/release.toml).
    #[serde(default)]
    accept: Vec<String>,
}

/// Where the program is unpacked before `zapretUI-svc.exe install` runs from there elevated. Not the
/// user's temp folder: programs without administrator rights can write there and swap the exe before
/// it starts. This folder is born with rights for administrators and the system only, inside
/// %SystemRoot%\Temp, where users can neither rename nor delete what is not theirs.
fn private_dir() -> anyhow::Result<std::path::PathBuf> {
    let root = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| r"C:\Windows".into());
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let path = root.join("Temp").join(format!("zapretUI-setup-{}-{nanos:08x}", std::process::id()));
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(w!("O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"), SDDL_REVISION_1, &mut sd, None)?;
        let attributes =
            SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: BOOL(0) };
        let made = CreateDirectoryW(&HSTRING::from(path.as_os_str()), Some(&attributes as *const SECURITY_ATTRIBUTES));
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        made.with_context(|| format!("папка {}", path.display()))?;
    }
    Ok(path)
}

fn install() -> anyhow::Result<()> {
    let exe = std::fs::read(std::env::current_exe()?)?;
    let (manifest_bytes, signature, body) = attached(&exe).and_then(unpack).context("Установщик повреждён: скачайте его заново")?;
    let source: Source = toml::from_str(RELEASE)?;
    let keys: Vec<Vec<u8>> =
        std::iter::once(&source.key).chain(&source.accept).filter_map(|k| unhex(k)).filter(|k| k.len() == 32).collect();
    ensure!(!keys.is_empty(), "в сборке нет ключа");
    let signed = keys.iter().any(|key| {
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key).verify(manifest_bytes, signature).is_ok()
    });
    ensure!(signed, "Подпись установщика не совпадает: скачайте его с GitHub заново");
    let manifest: Manifest = serde_json::from_slice(manifest_bytes)?;
    let parts = split(&manifest, body).context("Установщик повреждён: скачайте его заново")?;

    let dir = private_dir()?;
    for (entry, data) in manifest.files.iter().zip(parts) {
        ensure!(hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref()) == entry.sha256, "Установщик повреждён: {}", entry.path);
        let base = match entry.place {
            Place::Bin => dir.clone(),
            Place::Engine => dir.join("winws2"),
        };
        let path = entry.path.split('/').fold(base, |p, part| p.join(part));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, data).with_context(|| format!("распаковка {}", entry.path))?;
    }
    let output = std::process::Command::new(dir.join(paths::SERVICE_EXE))
        .arg("install")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("запуск установки");
    let _ = std::fs::remove_dir_all(&dir);
    let output = output?;
    if !output.status.success() {
        let text = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("Установка не удалась. {text}");
    }
    // Explorer starts the window with the signed-in user's rights, not this installer's.
    let explorer = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_default().join("explorer.exe");
    std::process::Command::new(explorer).arg(paths::app_exe()).spawn().context("запуск zapretUI")?;
    Ok(())
}

fn main() {
    if let Err(e) = install() {
        unsafe {
            MessageBoxW(None, &HSTRING::from(format!("{e:#}")), w!("zapretUI"), MB_OK | MB_ICONERROR);
        }
    }
}
