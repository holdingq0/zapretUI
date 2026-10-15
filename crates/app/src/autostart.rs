use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE: PCWSTR = w!("zapretUI");
const LEGACY: PCWSTR = w!("rkn");

pub fn enabled() -> bool {
    if let Some(root) = rkn_core::paths::portable_root() {
        return root.join("data").join("autostart.on").is_file();
    }
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set(enabled: bool) -> anyhow::Result<()> {
    if enabled {
        // The installed copy, even when the switch was flipped in a copy started from a download.
        let installed = rkn_core::paths::app_exe();
        let exe = if installed.is_file() { installed } else { std::env::current_exe()? };
        let command = format!("\"{}\" --tray", exe.display());
        let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                (wide.len() * 2) as u32,
            )
            .ok()?;
        }
    } else {
        let result = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) };
        if result != ERROR_FILE_NOT_FOUND {
            result.ok()?;
        }
    }
    Ok(())
}

/// Builds before the rename registered autostart as "rkn": move it to the new name and this exe.
pub fn migrate() {
    let legacy = unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, LEGACY, RRF_RT_REG_SZ, None, None, None).is_ok() };
    if legacy {
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, LEGACY);
        }
        let _ = set(true);
    }
}

pub fn light_taskbar() -> bool {
    personalize(w!("SystemUsesLightTheme"))
}

pub fn light_apps() -> bool {
    personalize(w!("AppsUseLightTheme"))
}

fn personalize(name: PCWSTR) -> bool {
    let mut value = 0u32;
    let mut size = 4u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            name,
            windows::Win32::System::Registry::RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    result.is_ok() && value == 1
}
