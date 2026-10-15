// What makes zapretUI an installed program for Windows: elevation, the Start menu shortcut,
// the entry in "Installed apps", closing the window before files go away.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use rkn_core::paths;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_DWORD, REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegSetKeyValueW};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{IShellLinkW, IsUserAnAdmin, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, MB_ICONINFORMATION, MB_OK, MessageBoxW, PostMessageW, SW_HIDE, WM_CLOSE};
use windows::core::{HSTRING, Interface, PCWSTR, w};

const UNINSTALL_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\zapretUI";
const DETACHED_PROCESS: u32 = 0x0000_0008;

pub fn is_admin() -> bool {
    unsafe { IsUserAnAdmin().as_bool() }
}

/// Runs this exe again with the same arguments through UAC and waits for it.
pub fn elevate(args: &[String]) -> anyhow::Result<()> {
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let params = HSTRING::from(args.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(" "));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).context("нужны права администратора")?;
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        if code != 0 {
            bail!("завершилось с кодом {code}");
        }
    }
    Ok(())
}

fn shortcut() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join(r"Microsoft\Windows\Start Menu\Programs")
        .join(format!("{}.lnk", paths::PRODUCT))
}

/// A Start menu entry for every user, pointing at the installed app.
pub fn add_shortcut() -> anyhow::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(paths::app_exe().as_os_str()))?;
        link.SetWorkingDirectory(&HSTRING::from(paths::install_dir().as_os_str()))?;
        link.SetDescription(w!("Обход блокировок для игр, Discord и YouTube"))?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(shortcut().as_os_str()), true)?;
    }
    Ok(())
}

fn set_string(key: &str, name: &str, value: &str) -> anyhow::Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    unsafe {
        RegSetKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(key), &HSTRING::from(name), REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32)
            .ok()?;
    }
    Ok(())
}

fn set_dword(key: &str, name: &str, value: u32) -> anyhow::Result<()> {
    unsafe {
        RegSetKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(key), &HSTRING::from(name), REG_DWORD.0, Some((&value as *const u32).cast()), 4).ok()?;
    }
    Ok(())
}

fn size_kb(dir: &Path) -> u32 {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum::<u64>() / 1024)
        .unwrap_or(0) as u32
}

/// The entry in "Installed apps": name, version, icon and the command that removes everything.
pub fn add_uninstall_entry() -> anyhow::Result<()> {
    let exe = paths::service_exe();
    set_string(UNINSTALL_KEY, "DisplayName", paths::PRODUCT)?;
    set_string(UNINSTALL_KEY, "DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    set_string(UNINSTALL_KEY, "Publisher", paths::PRODUCT)?;
    set_string(UNINSTALL_KEY, "DisplayIcon", &paths::app_exe().display().to_string())?;
    set_string(UNINSTALL_KEY, "InstallLocation", &paths::install_dir().display().to_string())?;
    set_string(UNINSTALL_KEY, "UninstallString", &format!("\"{}\" uninstall --purge --ui", exe.display()))?;
    set_dword(UNINSTALL_KEY, "NoModify", 1)?;
    set_dword(UNINSTALL_KEY, "NoRepair", 1)?;
    set_dword(UNINSTALL_KEY, "EstimatedSize", size_kb(&paths::install_dir()) + size_kb(&paths::engine_dir()))?;
    Ok(())
}

/// After an update the service starts as the new version: the entry follows it.
pub fn refresh_version() {
    let _ = set_string(UNINSTALL_KEY, "DisplayVersion", env!("CARGO_PKG_VERSION"));
}

pub fn remove_entries() {
    let _ = std::fs::remove_file(shortcut());
    unsafe {
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(UNINSTALL_KEY));
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("zapretUI"));
    }
}

/// Asks the running window to quit, the same way its tray menu does, and waits for it.
pub fn close_app() {
    let deadline = Instant::now() + Duration::from_secs(5);
    unsafe {
        while let Ok(window) = FindWindowW(w!("zapretUI.tray"), PCWSTR::null()) {
            let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
            if Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// A running exe cannot delete itself: a copy in the temp folder finishes the uninstall.
pub fn finish_from_temp(args: &[String]) -> anyhow::Result<bool> {
    let exe = std::env::current_exe()?;
    if !exe.starts_with(paths::install_dir()) {
        return Ok(false);
    }
    let copy = std::env::temp_dir().join(format!("zapretUI-uninstall-{}.exe", std::process::id()));
    std::fs::copy(&exe, &copy)?;
    std::process::Command::new(&copy).args(args).creation_flags(DETACHED_PROCESS).spawn()?;
    Ok(true)
}

/// Whatever is still locked goes at the next restart, the temp copy of the uninstaller included.
pub fn delete_later(path: &Path) {
    unsafe {
        let _ = MoveFileExW(&HSTRING::from(path.as_os_str()), PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT);
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Marker the window reads to show the switch; the task itself is the truth for Windows.
pub fn portable_autostart_mark() -> PathBuf {
    paths::data_dir().join("autostart.on")
}

/// Autostart of a portable copy: a logon task with the highest rights of `user`, so the window
/// and its service start without a UAC prompt. Creating or deleting the task needs elevation once.
pub fn portable_autostart(on: bool, user: &str) -> anyhow::Result<()> {
    use windows::Win32::System::TaskScheduler::{ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN, TaskScheduler};
    use windows::Win32::System::Variant::VARIANT;
    use windows::core::BSTR;
    anyhow::ensure!(paths::portable(), "только для портативной копии");
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let empty = VARIANT::default();
        service.Connect(&empty, &empty, &empty, &empty)?;
        let folder = service.GetFolder(&BSTR::from("\\"))?;
        let name = BSTR::from(paths::PRODUCT);
        if !on {
            let _ = folder.DeleteTask(&name, 0);
            let _ = std::fs::remove_file(portable_autostart_mark());
            return Ok(());
        }
        let user = xml(user);
        let task = format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>zapretUI, портативная версия: запуск при входе в Windows</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{app}</Command><Arguments>--tray</Arguments><WorkingDirectory>{dir}</WorkingDirectory></Exec></Actions>
</Task>"#,
            app = xml(&paths::app_exe().display().to_string()),
            dir = xml(&paths::install_dir().display().to_string()),
        );
        folder
            .RegisterTask(&name, &BSTR::from(task), TASK_CREATE_OR_UPDATE.0, &empty, &empty, TASK_LOGON_INTERACTIVE_TOKEN, &empty)
            .context("задание планировщика")?;
    }
    paths::write_atomic(&portable_autostart_mark(), "")?;
    Ok(())
}

pub fn message(text: &str) {
    unsafe {
        MessageBoxW(Some(HWND::default()), &HSTRING::from(text), w!("zapretUI"), MB_OK | MB_ICONINFORMATION);
    }
}
