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
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{HSTRING, w};

const RELEASE: &str = include_str!("../../../resources/release.toml");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Deserialize)]
struct Source {
    key: String,
}

fn install() -> anyhow::Result<()> {
    let exe = std::fs::read(std::env::current_exe()?)?;
    let (manifest_bytes, signature, body) = attached(&exe).and_then(unpack).context("Установщик повреждён: скачайте его заново")?;
    let source: Source = toml::from_str(RELEASE)?;
    let key = unhex(&source.key).context("в сборке нет ключа")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(manifest_bytes, signature)
        .map_err(|_| anyhow::anyhow!("Подпись установщика не совпадает: скачайте его с GitHub заново"))?;
    let manifest: Manifest = serde_json::from_slice(manifest_bytes)?;
    let parts = split(&manifest, body).context("Установщик повреждён: скачайте его заново")?;

    let dir = std::env::temp_dir().join(format!("zapretUI-setup-{}", std::process::id()));
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
