// zapretUI-setup.exe: one file with the release inside. Windows asks for administrator rights on
// launch (manifest); the files go to a temp folder, `zapretUI-svc.exe install` puts them in place,
// and the installed window opens with the user's own rights.
#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;

use anyhow::{Context, ensure};
use rkn_core::paths;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{HSTRING, w};

include!(concat!(env!("OUT_DIR"), "/payload.rs"));

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn install() -> anyhow::Result<()> {
    ensure!(!FILES.is_empty(), "Установщик собран без файлов программы");
    let dir = std::env::temp_dir().join(format!("zapretUI-setup-{}", std::process::id()));
    for (name, bytes) in FILES {
        let path = name.split('/').fold(dir.clone(), |p, part| p.join(part));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes).with_context(|| format!("распаковка {name}"))?;
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
