// The window never runs elevated itself: whatever needs administrator rights goes through
// zapretUI-svc.exe started with "runas" (one UAC prompt per action).

use rkn_core::paths;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, w};

/// Runs zapretUI-svc.exe from this copy's folder elevated. With `wait` it returns its exit status;
/// without it only whether Windows started it (the user may decline the prompt).
pub fn run_svc(params: &str, wait: bool) -> anyhow::Result<()> {
    let svc = std::env::current_exe()?.with_file_name(paths::SERVICE_EXE);
    anyhow::ensure!(svc.is_file(), "Рядом с программой нет {}", paths::SERVICE_EXE);
    let file = HSTRING::from(svc.as_os_str());
    let params = HSTRING::from(params);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: if wait { SEE_MASK_NOCLOSEPROCESS } else { Default::default() },
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|_| anyhow::anyhow!("Нужны права администратора: запрос Windows отклонён"))?;
        if wait {
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 0u32;
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
            anyhow::ensure!(code == 0, "Не получилось (код {code})");
        }
    }
    Ok(())
}

/// DOMAIN\user of whoever runs the window: the elevated helper may run as another account.
pub fn user() -> String {
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let name = std::env::var("USERNAME").unwrap_or_default();
    if domain.is_empty() { name } else { format!("{domain}\\{name}") }
}
