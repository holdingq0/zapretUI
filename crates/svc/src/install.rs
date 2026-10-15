use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use rkn_core::ipc::{LEGACY_SERVICE_NAME, SERVICE_NAME};
use rkn_core::paths;
use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl, ServiceFailureActions,
    ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

use crate::{acl, engine, system};

fn source_dir(exe: &Path) -> anyhow::Result<PathBuf> {
    let dir = exe.parent().context("exe path")?;
    [dir.join("winws2"), dir.join("../../resources/winws2")]
        .into_iter()
        .find(|p| p.join("winws2.exe").is_file())
        .context("рядом с zapretUI-svc.exe нет папки winws2")
}

/// Copies a file into Program Files. A copy that is running there is moved aside first.
fn place(from: &Path, to: &Path) -> anyhow::Result<()> {
    if from.to_string_lossy().eq_ignore_ascii_case(&to.to_string_lossy()) {
        return Ok(());
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut moved = None;
    if to.exists() && std::fs::remove_file(to).is_err() {
        let mut old = to.as_os_str().to_owned();
        old.push(".old");
        let old = PathBuf::from(old);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(to, &old).with_context(|| format!("{} занят", to.display()))?;
        moved = Some(old);
    }
    if let Err(e) = std::fs::copy(from, to) {
        // The running copy goes back: a failed install must not leave the service without its exe.
        if let Some(old) = moved {
            let _ = std::fs::rename(old, to);
        }
        return Err(e).with_context(|| format!("копирование {}", to.display()));
    }
    Ok(())
}

fn copy_binaries(exe: &Path) -> anyhow::Result<PathBuf> {
    let dst = paths::service_exe();
    place(exe, &dst)?;
    let app = exe.with_file_name(paths::APP_EXE);
    if app.is_file() {
        place(&app, &paths::app_exe())?;
    } else {
        println!("Рядом нет {}: окно не установлено", paths::APP_EXE);
    }
    Ok(dst)
}

/// Builds before the rename ran as the service "rkn" with data in ProgramData/rkn.
fn migrate(manager: &ServiceManager) -> anyhow::Result<()> {
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    if let Ok(old) = manager.open_service(LEGACY_SERVICE_NAME, access) {
        stop(&old)?;
        old.delete()?;
        engine::unload_driver();
        println!("Старая служба {LEGACY_SERVICE_NAME} удалена");
    }
    let (old, new) = (paths::legacy_data_dir(), paths::data_dir());
    if old.is_dir() && !new.exists() {
        std::fs::rename(&old, &new).with_context(|| format!("перенос {}", old.display()))?;
        let _ = std::fs::remove_file(new.join("rkn-svc.log"));
        let _ = std::fs::remove_dir_all(new.join("bin"));
        println!("Данные перенесены в {}", new.display());
    }
    Ok(())
}

fn wait_stopped(service: &Service) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while service.query_status()?.current_state != ServiceState::Stopped {
        if Instant::now() > deadline {
            bail!("служба {SERVICE_NAME} не остановилась за 20 с");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

fn stop(service: &Service) -> anyhow::Result<()> {
    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop()?;
        wait_stopped(service)?;
    }
    Ok(())
}

fn copy_engine(src: &Path) -> anyhow::Result<()> {
    let dst = paths::engine_dir();
    let names = engine::manifest().map(|(_, n)| n).chain(engine::EXTRA_FILES);
    for name in names {
        let from = src.join(name);
        if !from.is_file() {
            continue;
        }
        let to = dst.join(name);
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(&from, &to).with_context(|| format!("копирование {name}"))?;
    }
    Ok(())
}

pub fn install() -> anyhow::Result<()> {
    if paths::portable() {
        bail!("Это портативная копия: она работает без установки");
    }
    if !system::is_admin() {
        return system::elevate(&["install".into()]);
    }
    let exe = std::env::current_exe()?;
    let src = source_dir(&exe)?;
    engine::verify(&src).context("исходные файлы winws2")?;

    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("нужны права администратора")?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::START | ServiceAccess::CHANGE_CONFIG;
    let existing = manager.open_service(SERVICE_NAME, access).ok();
    if let Some(service) = &existing {
        stop(service)?;
    }
    let prepared = (|| {
        migrate(&manager)?;
        acl::secure(&paths::data_dir()).context("права на папку данных")?;
        copy_engine(&src)?;
        engine::verify(&paths::engine_dir()).context("установленные файлы winws2")?;
        copy_binaries(&exe)
    })();
    let exe = match prepared {
        Ok(exe) => exe,
        Err(e) => {
            // An antivirus holding a file, say: the version that was there keeps working.
            if let Some(service) = &existing {
                let _ = service.start::<&str>(&[]);
            }
            return Err(e);
        }
    };

    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(paths::PRODUCT),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    let service = match existing {
        Some(service) => {
            service.change_config(&info)?;
            service
        }
        None => manager.create_service(&info, access)?,
    };
    service.set_description("zapretUI: находит заблокированные сайты и сам подбирает способ обхода")?;
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![
            ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(5) },
            ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(30) },
            ServiceAction { action_type: ServiceActionType::None, delay: Duration::ZERO },
        ]),
    })?;
    service.start::<&str>(&[])?;
    if let Err(e) = system::add_shortcut() {
        println!("Ярлык в меню «Пуск» не создан: {e:#}");
    }
    if let Err(e) = system::add_uninstall_entry() {
        println!("Запись в списке программ не создана: {e:#}");
    }
    println!("Служба {SERVICE_NAME} установлена и запущена. Программа: {}", paths::install_dir().display());
    Ok(())
}

/// The service is installed but stopped (it gave up after crashes, or someone stopped it): the
/// window's button starts it, through UAC because users may not start services.
pub fn start() -> anyhow::Result<()> {
    if !system::is_admin() {
        return system::elevate(&["start".into()]);
    }
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::START)?;
    if service.query_status()?.current_state == ServiceState::Stopped {
        service.start::<&str>(&[])?;
    }
    Ok(())
}

/// Run by the service itself after an update: stops it and starts the new binary. If the new
/// service does not stay up, the files the update moved aside come back and the old one starts.
pub fn restart() -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::START)?;
    std::thread::sleep(Duration::from_millis(500));
    // A slow stop must not leave the service off: give it one more wait, then start regardless.
    if stop(&service).is_err() {
        let _ = wait_stopped(&service);
    }
    if service.start::<&str>(&[]).is_ok() && stays_up(&service) {
        return Ok(());
    }
    let _ = stop(&service);
    crate::update::roll_back();
    service.start::<&str>(&[])?;
    Ok(())
}

/// Running ten seconds after the start, the way it should be.
fn stays_up(service: &Service) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut up_since: Option<Instant> = None;
    while Instant::now() < deadline {
        match service.query_status().map(|s| s.current_state) {
            Ok(ServiceState::Running) => {
                if up_since.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(10) {
                    return true;
                }
            }
            Ok(ServiceState::Stopped) | Err(_) => return false,
            Ok(_) => up_since = None,
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn remove_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while path.exists() && std::fs::remove_file(path).is_err() {
        if Instant::now() > deadline {
            system::delete_later(path);
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// From "Installed apps" (`--purge --ui`) or by hand: service, files, entries, and with --purge the data.
pub fn uninstall(args: &[String]) -> anyhow::Result<()> {
    if paths::portable() {
        bail!("Это портативная копия: удалите её папку");
    }
    if !system::is_admin() {
        return system::elevate(args);
    }
    if system::finish_from_temp(args)? {
        return Ok(());
    }
    let purge = args.iter().any(|a| a == "--purge");
    system::close_app();
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).context("нужны права администратора")?;
    if let Ok(service) = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE) {
        stop(&service)?;
        service.delete()?;
        println!("Служба {SERVICE_NAME} удалена");
    }
    engine::unload_driver();
    system::remove_entries();
    let dir = paths::install_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            remove_file(&entry.path());
        }
    }
    if std::fs::remove_dir(&dir).is_err() && dir.exists() {
        system::delete_later(&dir);
    }
    if purge {
        let data = paths::data_dir();
        if data.exists() {
            std::fs::remove_dir_all(&data).with_context(|| format!("удаление {}", data.display()))?;
        }
    }
    let me = std::env::current_exe()?;
    if me.starts_with(std::env::temp_dir()) {
        system::delete_later(&me);
    }
    if args.iter().any(|a| a == "--ui") {
        system::message("zapretUI удалён");
    }
    Ok(())
}
