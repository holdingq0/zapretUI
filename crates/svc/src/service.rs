use std::ffi::OsString;
use std::time::Duration;

use rkn_core::ipc::SERVICE_NAME;
use rkn_core::paths;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::{acl, app, logging};

define_windows_service!(ffi_service_main, service_main);

pub fn dispatch() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()
}

pub fn run_console() -> anyhow::Result<()> {
    runtime()?.block_on(app::run(async {
        let _ = tokio::signal::ctrl_c().await;
    }))
}

/// The portable copy: started elevated by its window, alive while the window is. `after` is the
/// previous service that is handing over after an update; the pipe is free once it is gone.
pub fn run_portable(after: Option<u32>) -> anyhow::Result<()> {
    anyhow::ensure!(paths::portable(), "команда только для портативной копии");
    logging::init(false);
    if let Some(pid) = after {
        wait_exit(pid);
    }
    runtime()?.block_on(app::run(window_gone()))
}

fn wait_exit(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    unsafe {
        if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(process, 20_000);
            let _ = CloseHandle(process);
        }
    }
}

/// The window holds the mutex Local\zapretUI.app for as long as it runs, across its own handovers.
fn window_alive() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    use windows::core::w;
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

async fn window_gone() {
    let mut missing = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        missing = if window_alive() { 0 } else { missing + 1 };
        if missing >= 3 {
            tracing::info!("window closed, portable copy stops");
            return;
        }
    }
}

fn service_main(_args: Vec<OsString>) {
    logging::init(false);
    if let Err(e) = run_service() {
        tracing::error!("{e:#}");
    }
}

fn report(state: ServiceState, code: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    }
}

fn run_service() -> anyhow::Result<()> {
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let handle = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = tx.send(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    handle.set_service_status(report(ServiceState::Running, 0))?;
    crate::system::refresh_version();
    if let Err(e) = acl::secure(&paths::data_dir()) {
        handle.set_service_status(report(ServiceState::Stopped, 1))?;
        return Err(e);
    }
    let result = runtime().map_err(anyhow::Error::from).and_then(|rt| {
        rt.block_on(app::run(async move {
            let _ = rx.wait_for(|stop| *stop).await;
        }))
    });
    handle.set_service_status(report(ServiceState::StopPending, 0))?;
    handle.set_service_status(report(ServiceState::Stopped, if result.is_ok() { 0 } else { 1 }))?;
    result
}
