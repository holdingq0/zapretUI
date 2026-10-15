use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use rkn_core::ipc::{Message, PIPE_NAME, Request, Status, encode};

/// What the service last told us. The UI thread reads it on every repaint.
#[derive(Default)]
pub struct Shared {
    pub connected: bool,
    /// Whether the service is registered at all; known after the pipe failed to open.
    pub installed: Option<bool>,
    pub status: Option<Status>,
}

pub type Store = Arc<Mutex<Shared>>;

const ERROR_PIPE_BUSY: i32 = 231;

fn open() -> std::io::Result<File> {
    let path = format!(r"\\.\pipe\{PIPE_NAME}");
    let mut tries = 0;
    loop {
        match OpenOptions::new().read(true).write(true).open(&path) {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

/// One request on its own connection. Blocking: call it from a worker thread.
pub fn request(request: &Request) -> anyhow::Result<Message> {
    let mut pipe = open().context("служба zapretUI не отвечает")?;
    pipe.write_all(encode(request).as_bytes())?;
    let mut line = String::new();
    BufReader::new(pipe).read_line(&mut line)?;
    if line.is_empty() {
        bail!("служба закрыла соединение");
    }
    Ok(serde_json::from_str(&line)?)
}

/// Follows the service forever on a background thread; `changed` runs after every update.
pub fn watch(store: Store, changed: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        loop {
            let _ = follow(&store, &changed);
            let installed = service_installed();
            {
                let mut shared = store.lock().unwrap_or_else(|e| e.into_inner());
                shared.connected = false;
                shared.status = None;
                shared.installed = Some(installed);
            }
            changed();
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

fn service_installed() -> bool {
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
    };
    use windows::core::{HSTRING, PCWSTR};
    unsafe {
        let Ok(manager) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) else { return true };
        let found = OpenServiceW(manager, &HSTRING::from(rkn_core::ipc::SERVICE_NAME), SERVICE_QUERY_STATUS);
        if let Ok(service) = &found {
            let _ = CloseServiceHandle(*service);
        }
        let _ = CloseServiceHandle(manager);
        found.is_ok()
    }
}

fn follow(store: &Store, changed: &impl Fn()) -> anyhow::Result<()> {
    let mut pipe = open()?;
    pipe.write_all(encode(&Request::Subscribe).as_bytes())?;
    {
        let mut shared = store.lock().unwrap_or_else(|e| e.into_inner());
        shared.connected = true;
        shared.installed = Some(true);
    }
    let mut reader = BufReader::new(pipe);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        // A newer service may send something this copy does not know yet: skip it, keep following.
        let Ok(message) = serde_json::from_str::<Message>(&line) else { continue };
        {
            if let Message::Status(status) = message {
                store.lock().unwrap_or_else(|e| e.into_inner()).status = Some(*status);
            }
        }
        if reader.buffer().is_empty() {
            changed();
        }
    }
}
