#![windows_subsystem = "windows"]
// Drawing helpers take position, size, colour and alpha as plain arguments.
#![allow(clippy::too_many_arguments)]

mod admin;
mod autostart;
mod field;
mod gfx;
mod icons;
mod pipe;
mod pixel;
mod svg;
mod text;
mod theme;
mod tray;
mod ui;

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use rkn_core::config::Config;
use rkn_core::paths;
use rkn_core::ipc::{Message, Request, State, UpdateState};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::{D2D1_RENDER_TARGET_TYPE, D2D1_RENDER_TARGET_TYPE_SOFTWARE};
use windows::Win32::Graphics::Dwm::{
    DWM_WINDOW_CORNER_PREFERENCE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmFlush,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient, ValidateRect};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::Foundation::WAIT_TIMEOUT;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, WaitForSingleObject};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::pipe::Store;
use crate::theme::Theme;
use crate::ui::{Act, Pace, Screen, Ui};

const TRAY_CLASS: PCWSTR = w!("zapretUI.tray");
const MAIN_CLASS: PCWSTR = w!("zapretUI.main");
const WM_UPDATE: u32 = WM_APP + 1;
const WM_REPLY: u32 = WM_APP + 3;
const WM_SHOW: u32 = WM_APP + 4;
const WM_HANDOVER: u32 = WM_APP + 5;
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;
const WM_MOUSELEAVE: u32 = 0x02a3;

type Proc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
// Software rendering keeps the GPU driver out of the process: a few MB instead of tens.
const RENDER: D2D1_RENDER_TARGET_TYPE = D2D1_RENDER_TARGET_TYPE_SOFTWARE;

struct Main {
    hwnd: HWND,
    gfx: gfx::Gfx,
    canvas: Option<gfx::Canvas>,
    ui: Ui,
    dpi: u32,
    pace: Pace,
    tracking: bool,
}

struct App {
    store: Store,
    tray_hwnd: HWND,
    tray: tray::Tray,
    main: Option<Main>,
    theme: Theme,
    config: Option<Config>,
    autostart: bool,
    /// A GetConfig is on its way; not asked again until it answers.
    config_asked: bool,
    taskbar_created: u32,
    /// A newer copy is taking over; ignore further triggers until we are gone.
    leaving: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app state unless a re-entrant window message already holds it.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut app| app.as_mut().map(f)))
}

enum Tag {
    Plain,
    Config,
    Saved,
    Installed,
    Autostart,
}

struct Reply {
    tag: Tag,
    result: anyhow::Result<Message>,
}

/// Sends a request on a worker thread; the answer comes back to the tray window as WM_REPLY.
fn send(tray: HWND, tag: Tag, request: Request) {
    let target = tray.0 as isize;
    std::thread::spawn(move || {
        let reply = Box::new(Reply { tag, result: pipe::request(&request) });
        unsafe {
            let raw = Box::into_raw(reply);
            if PostMessageW(Some(HWND(target as *mut _)), WM_REPLY, WPARAM(0), LPARAM(raw as isize)).is_err() {
                drop(Box::from_raw(raw));
            }
        }
    });
}

/// The dot-matrix mark baked into the .exe by build.rs, at the size Windows asks for.
fn app_icon(metric: SYSTEM_METRICS_INDEX) -> HICON {
    unsafe {
        let size = GetSystemMetrics(metric);
        let module = GetModuleHandleW(None).unwrap_or_default();
        LoadImageW(Some(module.into()), PCWSTR(std::ptr::without_provenance(1)), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
            .map(|h| HICON(h.0))
            .unwrap_or_default()
    }
}

/// Starts the installed copy to take over from this one, then quits; the new copy waits for our mutex.
fn hand_over(tray: bool, updated: bool) {
    if with(|app| std::mem::replace(&mut app.leaving, true)).unwrap_or(true) {
        return;
    }
    let mut cmd = std::process::Command::new(paths::app_exe());
    cmd.arg("--replace");
    if updated {
        cmd.arg("--updated");
    }
    if tray {
        cmd.arg("--tray");
    }
    if cmd.spawn().is_ok() {
        quit();
    } else {
        with(|app| app.leaving = false);
    }
}

/// Native menus follow the dark app theme only after this undocumented uxtheme call (ordinals 135/136),
/// the same one Explorer and Windows Terminal rely on. Without it the tray menu stays light.
fn dark_menus() {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::PCSTR;
    unsafe {
        let Ok(module) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(set) = GetProcAddress(module, PCSTR(std::ptr::without_provenance(135))) {
            let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set);
            set(1);
        }
        if let Some(flush) = GetProcAddress(module, PCSTR(std::ptr::without_provenance(136))) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
    }
}

/// The portable copy has no installed service: its own one runs elevated next to it while the window lives.
/// Nothing happens when a service already answers (the portable copy started twice, or an installed one).
fn launch_portable(tray: HWND) {
    let target = tray.0 as isize;
    std::thread::spawn(move || {
        if pipe::request(&Request::Status).is_ok() {
            return;
        }
        reply(HWND(target as *mut _), Tag::Plain, admin::run_svc("portable", false).map(|_| Message::Ok));
    });
}

/// Posts a worker's answer to the tray window, which handles it as WM_REPLY.
fn reply(tray: HWND, tag: Tag, result: anyhow::Result<Message>) {
    let reply = Box::new(Reply { tag, result });
    unsafe {
        let raw = Box::into_raw(reply);
        if PostMessageW(Some(tray), WM_REPLY, WPARAM(0), LPARAM(raw as isize)).is_err() {
            drop(Box::from_raw(raw));
        }
    }
}

/// Elevated `zapretUI-svc.exe start`: the service is installed but stopped.
fn start_service(tray: HWND) {
    let target = tray.0 as isize;
    std::thread::spawn(move || reply(HWND(target as *mut _), Tag::Plain, admin::run_svc("start", true).map(|_| Message::Ok)));
}

/// Elevated `zapretUI-svc.exe install` from the folder this copy runs in.
fn install(tray: HWND) {
    let target = tray.0 as isize;
    std::thread::spawn(move || reply(HWND(target as *mut _), Tag::Installed, admin::run_svc("install", true).map(|_| Message::Ok)));
}

/// Autostart: a registry entry for the installed copy; a logon task for the portable one,
/// which asks for administrator rights once, here.
fn toggle_autostart(tray: HWND, on: bool) {
    let target = tray.0 as isize;
    std::thread::spawn(move || {
        let result = if paths::portable() {
            admin::run_svc(&format!("autostart {} --user \"{}\"", if on { "on" } else { "off" }, admin::user()), true)
        } else {
            autostart::set(on)
        };
        reply(HWND(target as *mut _), Tag::Autostart, result.map(|_| Message::Ok));
    });
}

fn trim() {
    unsafe {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}

fn loword(v: usize) -> u32 {
    (v & 0xffff) as u32
}

fn xy(l: LPARAM) -> (i32, i32) {
    ((l.0 & 0xffff) as i16 as i32, ((l.0 >> 16) & 0xffff) as i16 as i32)
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let args: Vec<String> = std::env::args().collect();
    let tray_only = args.iter().any(|a| a == "--tray");
    let replace = args.iter().any(|a| a == "--replace");

    // One copy only: a second launch just shows the window of the first.
    // A copy started with --replace (after an install or update) waits for the old one to quit instead.
    let single = unsafe { CreateMutexW(None, true, w!("Local\\zapretUI.app")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let taken = replace && single.as_ref().is_ok_and(|h| unsafe { WaitForSingleObject(*h, 15_000) } != WAIT_TIMEOUT);
        if !taken {
            unsafe {
                if let Ok(h) = FindWindowW(TRAY_CLASS, PCWSTR::null()) {
                    let _ = PostMessageW(Some(h), WM_SHOW, WPARAM(0), LPARAM(0));
                }
            }
            return;
        }
    }
    if !paths::portable() {
        autostart::migrate();
    }
    dark_menus();

    let hinstance = unsafe { GetModuleHandleW(None).unwrap_or_default() };
    unsafe {
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let procs: [(PCWSTR, Proc); 2] = [(TRAY_CLASS, tray_proc), (MAIN_CLASS, main_proc)];
        for (name, proc_) in procs {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc_),
                hInstance: hinstance.into(),
                hCursor: cursor,
                hIcon: app_icon(SM_CXICON),
                hIconSm: app_icon(SM_CXSMICON),
                lpszClassName: name,
                ..Default::default()
            };
            RegisterClassExW(&class);
        }
    }
    let tray_hwnd = unsafe {
        CreateWindowExW(WINDOW_EX_STYLE(0), TRAY_CLASS, w!("zapretUI"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(hinstance.into()), None)
            .expect("tray window")
    };

    let store: Store = Arc::new(Mutex::new(pipe::Shared::default()));
    APP.with(|cell| {
        *cell.borrow_mut() = Some(App {
            store: store.clone(),
            tray_hwnd,
            tray: tray::Tray::new(tray_hwnd),
            main: None,
            theme: theme::current(),
            config: None,
            autostart: autostart::enabled(),
            config_asked: false,
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            leaving: false,
        })
    });
    with(|app| {
        let look = tray::look(&app.store.lock().unwrap_or_else(|e| e.into_inner()));
        app.tray.add(look);
    });
    let target = tray_hwnd.0 as isize;
    pipe::watch(store, move || unsafe {
        let _ = PostMessageW(Some(HWND(target as *mut _)), WM_UPDATE, WPARAM(0), LPARAM(0));
    });
    if paths::portable() {
        launch_portable(tray_hwnd);
    }

    if !tray_only {
        open_main();
    }
    trim();

    // Messages first; while anything animates, frames follow the display's refresh (DwmFlush), not a timer.
    unsafe {
        let mut msg = MSG::default();
        'run: loop {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'run;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if !frame() {
                let _ = WaitMessage();
            }
        }
    }
    APP.with(|cell| cell.borrow_mut().take());
}

fn main_hwnd() -> Option<HWND> {
    with(|app| app.main.as_ref().map(|m| m.hwnd)).flatten()
}

fn open_main() {
    unsafe {
        if let Some(h) = main_hwnd() {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
            return;
        }
        let dpi = GetDpiForSystem();
        let (w, h) = ((ui::W * dpi as f32 / 96.0).round() as i32, (ui::H * dpi as f32 / 96.0).round() as i32);
        let mut area = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some((&mut area as *mut RECT).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let x = area.left + (area.right - area.left - w) / 2;
        let y = area.top + (area.bottom - area.top - h) / 2;
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_APPWINDOW,
            MAIN_CLASS,
            w!("zapretUI"),
            WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinstance.into()),
            None,
        ) else {
            return;
        };
        // Re-run WM_NCCALCSIZE now that our handler is in place, so the system frame disappears.
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        // The system DPI is the one from sign-in; the monitor may have been rescaled since.
        let real = GetDpiForWindow(hwnd);
        if real != dpi && real != 0 {
            let (w2, h2) = ((ui::W * real as f32 / 96.0).round() as i32, (ui::H * real as f32 / 96.0).round() as i32);
            let (x2, y2) = (area.left + (area.right - area.left - w2) / 2, area.top + (area.bottom - area.top - h2) / 2);
            let _ = SetWindowPos(hwnd, None, x2, y2, w2, h2, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, (&corner as *const DWM_WINDOW_CORNER_PREFERENCE).cast(), size_of_val(&corner) as u32);
        let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 });
        let Ok(gfx) = gfx::Gfx::new() else {
            let _ = DestroyWindow(hwnd);
            return;
        };
        with(|app| {
            let dark = BOOL::from(app.theme.dark);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&dark as *const BOOL).cast(), size_of::<BOOL>() as u32);
            app.main = Some(Main { hwnd, gfx, canvas: None, ui: Ui::new(), dpi: GetDpiForWindow(hwnd), pace: Pace::Smooth, tracking: false });
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn close_main() {
    if let Some(h) = main_hwnd() {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

/// "Выход": a portable copy also stops its service, an installed one leaves the service running.
fn exit() {
    if paths::portable() {
        // A busy service must not freeze the tray: it also stops by itself a few seconds after the window is gone.
        let (done, wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = pipe::request(&Request::Shutdown);
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(std::time::Duration::from_millis(1500));
    }
    quit();
}

fn quit() {
    close_main();
    if let Some(h) = with(|app| app.tray_hwnd) {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

fn invalidate(main: &mut Main) {
    unsafe {
        let _ = InvalidateRect(Some(main.hwnd), None, false);
    }
}

fn refresh_theme(app: &mut App) {
    app.theme = theme::current();
    let look = tray::look(&app.store.lock().unwrap_or_else(|e| e.into_inner()));
    app.tray.update(look);
    if let Some(main) = &mut app.main {
        main.ui.theme_changed(app.theme);
        let dark = BOOL::from(app.theme.dark);
        unsafe {
            let _ = DwmSetWindowAttribute(main.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&dark as *const BOOL).cast(), size_of::<BOOL>() as u32);
        }
        invalidate(main);
    }
}

fn on_update(app: &mut App) {
    let (look, version) = {
        let shared = app.store.lock().unwrap_or_else(|e| e.into_inner());
        let version = shared.status.as_ref().filter(|_| shared.connected).map(|s| s.version.clone());
        (tray::look(&shared), version)
    };
    // The service updated itself and the app next to it: hand over to the new copy, once.
    if let Some(version) = &version {
        let fresh = std::env::args().any(|a| a == "--updated");
        if paths::newer(version, env!("CARGO_PKG_VERSION")) && !fresh && paths::app_exe().is_file() {
            let tray = app.main.is_none();
            unsafe {
                let _ = PostMessageW(Some(app.tray_hwnd), WM_HANDOVER, WPARAM(tray as usize), LPARAM(1));
            }
        }
    }
    app.tray.update(look);
    // Settings opened while the service was starting: the config request failed then, ask again now.
    if app.config.is_none() && !app.config_asked && version.is_some() && app.main.as_ref().is_some_and(|m| m.ui.screen == Screen::Settings) {
        app.config_asked = true;
        send(app.tray_hwnd, Tag::Config, Request::GetConfig);
    }
    if let Some(main) = &mut app.main {
        invalidate(main);
    }
}

fn on_reply(app: &mut App, reply: Reply) {
    let problem = match &reply.result {
        Ok(Message::Error { message }) => Some(message.clone()),
        Err(e) => Some(format!("{e:#}")),
        _ => None,
    };
    let installed = matches!(reply.tag, Tag::Installed) && problem.is_none();
    if matches!(reply.tag, Tag::Autostart) {
        app.autostart = autostart::enabled();
    }
    if matches!(reply.tag, Tag::Config) {
        app.config_asked = false;
    }
    match (reply.tag, reply.result) {
        (Tag::Config, Ok(Message::Config(config))) => app.config = Some(config),
        (Tag::Saved, _) if problem.is_some() => send(app.tray_hwnd, Tag::Config, Request::GetConfig),
        _ => {}
    }
    // Installed from a downloaded folder: the copy in Program Files takes over (autostart points there).
    if installed {
        let here = std::env::current_exe().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
        let target = paths::app_exe();
        if target.is_file() && here != target.to_string_lossy().to_lowercase() {
            unsafe {
                let _ = PostMessageW(Some(app.tray_hwnd), WM_HANDOVER, WPARAM(0), LPARAM(0));
            }
        }
    }
    if let Some(main) = &mut app.main {
        if let Some(text) = problem {
            main.ui.say(text);
        }
        invalidate(main);
    }
}

/// Runs what a click asked for. Window-level calls are returned to run after the state is released.
fn act(app: &mut App, act: Act) -> Option<Act> {
    let tray = app.tray_hwnd;
    match act {
        Act::None => {}
        Act::Minimize | Act::Close => return Some(act),
        Act::SetEnabled(enabled) => send(tray, Tag::Plain, Request::SetEnabled { enabled }),
        Act::Relearn => send(tray, Tag::Plain, Request::Relearn),
        Act::StopLearning => send(tray, Tag::Plain, Request::StopLearning),
        Act::Install if paths::portable() => launch_portable(tray),
        Act::Install if app.store.lock().unwrap_or_else(|e| e.into_inner()).installed == Some(true) => start_service(tray),
        Act::Install => install(tray),
        Act::CheckUpdate => send(tray, Tag::Plain, Request::CheckUpdate),
        Act::InstallUpdate => send(tray, Tag::Plain, Request::InstallUpdate),
        Act::LoadConfig => {
            app.autostart = autostart::enabled();
            send(tray, Tag::Config, Request::GetConfig);
        }
        Act::Toggle(0) => toggle_autostart(tray, !app.autostart),
        Act::Toggle(_) => {
            let mut config = app.config.clone()?;
            config.auto_update = !config.auto_update;
            app.config = Some(config.clone());
            send(tray, Tag::Saved, Request::SetConfig { config });
        }
    }
    None
}

fn after(hwnd: HWND, act: Option<Act>) {
    unsafe {
        match act {
            Some(Act::Minimize) => {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
            Some(Act::Close) => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

fn paint(app: &mut App) {
    let App { store, main, theme, config, autostart, .. } = app;
    let Some(main) = main else { return };
    let shared = store.lock().unwrap_or_else(|e| e.into_inner());
    main.ui.missing = !shared.connected && (paths::portable() || shared.installed == Some(false));
    main.ui.stopped = !shared.connected && !paths::portable() && shared.installed == Some(true);
    main.ui.sync(shared.connected, shared.status.as_ref(), config.as_ref(), *autostart);
    if main.canvas.is_none() {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(main.hwnd, &mut r);
        }
        main.canvas = gfx::Canvas::new(&main.gfx, main.hwnd, (r.right - r.left) as u32, (r.bottom - r.top) as u32, main.dpi as f32, RENDER).ok();
    }
    let scale = main.dpi as f32 / 96.0;
    if let Some(canvas) = &mut main.canvas {
        main.ui.paint(canvas, &main.gfx, theme, scale, shared.connected, shared.status.as_ref(), config.as_ref());
    }
    // Windows may make the window active inside this app without letting it to the front (another app
    // holds the focus): only the real foreground window animates.
    main.ui.focused = unsafe { GetForegroundWindow() } == main.hwnd;
    main.pace = if unsafe { IsIconic(main.hwnd).as_bool() } { Pace::Still } else { main.ui.pace(shared.status.as_ref()) };
}

/// Paints one frame when something moves and waits for the next screen refresh.
/// Returns false when nothing needs frames, so the loop can sleep until a message arrives.
fn frame() -> bool {
    let Some(pace) = with(|app| app.main.as_ref().map(|m| m.pace)).flatten() else { return false };
    if pace == Pace::Still {
        return false;
    }
    with(paint);
    if pace == Pace::Smooth && unsafe { DwmFlush() }.is_ok() {
        return true;
    }
    // Ambient frames (24 fps, like the field): wait for the next one, but wake up at once for input.
    unsafe {
        let _ = MsgWaitForMultipleObjects(None, false, 41, QS_ALLINPUT);
    }
    true
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_UPDATE => {
                with(on_update);
                LRESULT(0)
            }
            WM_REPLY => {
                let reply = Box::from_raw(lparam.0 as *mut Reply);
                with(|app| on_reply(app, *reply));
                LRESULT(0)
            }
            WM_SHOW => {
                open_main();
                LRESULT(0)
            }
            WM_HANDOVER => {
                hand_over(wparam.0 != 0, lparam.0 != 0);
                LRESULT(0)
            }
            tray::MESSAGE => {
                match loword(lparam.0 as usize) {
                    NIN_SELECT | NIN_KEYSELECT => {
                        let front = main_hwnd().is_some_and(|h| GetForegroundWindow() == h && !IsIconic(h).as_bool());
                        if front { close_main() } else { open_main() }
                    }
                    WM_CONTEXTMENU => menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                with(refresh_theme);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ if with(|app| app.taskbar_created == msg && msg != 0).unwrap_or(false) => {
                with(|app| {
                    let look = tray::look(&app.store.lock().unwrap_or_else(|e| e.into_inner()));
                    app.tray.add(look);
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[derive(Clone, Copy)]
enum Item {
    Open = 1,
    Power,
    Learn,
    Update,
    Autostart,
    Quit,
}

const ITEMS: [Item; 6] = [Item::Open, Item::Power, Item::Learn, Item::Update, Item::Autostart, Item::Quit];

fn menu(hwnd: HWND) {
    let (status, autostart) = with(|app| {
        let shared = app.store.lock().unwrap_or_else(|e| e.into_inner());
        (shared.status.clone().filter(|_| shared.connected), autostart::enabled())
    })
    .unwrap_or((None, false));
    let state = status.as_ref().map(|s| s.state);
    let update = status.as_ref().map(|s| s.update.clone()).unwrap_or_default();
    let connected = status.is_some();
    let paused = state == Some(State::Paused);
    let learning = state == Some(State::Learning);
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let add = |flags: MENU_ITEM_FLAGS, item: Option<Item>, text: &str| {
            let text = wide(text);
            let _ = AppendMenuW(m, flags, item.map_or(0, |i| i as usize), PCWSTR(text.as_ptr()));
        };
        let on = |ok: bool| if ok { MF_STRING } else { MF_STRING | MF_GRAYED };
        let look = with(|app| tray::look(&app.store.lock().unwrap_or_else(|e| e.into_inner()))).map(|l| l.label).unwrap_or("");
        add(MF_STRING | MF_GRAYED, None, &format!("zapretUI {}: {look}", env!("CARGO_PKG_VERSION")));
        add(MF_SEPARATOR, None, "");
        add(MF_STRING | MF_DEFAULT, Some(Item::Open), "Открыть");
        add(on(connected), Some(Item::Power), if paused { "Включить обход" } else { "Выключить обход" });
        add(on(connected && !paused), Some(Item::Learn), if learning { "Остановить настройку" } else { "Настроить заново" });
        add(MF_SEPARATOR, None, "");
        let (text, usable) = match update.state {
            UpdateState::Available => (format!("Обновить до {}", update.version.clone().unwrap_or_default()), true),
            UpdateState::Checking => ("Проверяю обновления…".to_string(), false),
            UpdateState::Downloading => ("Скачиваю обновление…".to_string(), false),
            UpdateState::Installing => ("Устанавливаю обновление…".to_string(), false),
            UpdateState::Off => ("Обновления выключены в этой сборке".to_string(), false),
            _ => ("Проверить обновления".to_string(), true),
        };
        add(on(connected && usable), Some(Item::Update), &text);
        add(if autostart { MF_STRING | MF_CHECKED } else { MF_STRING }, Some(Item::Autostart), "Запускать с Windows");
        add(MF_SEPARATOR, None, "");
        add(MF_STRING, Some(Item::Quit), "Выход");
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        let Some(item) = ITEMS.into_iter().find(|i| *i as i32 == cmd.0) else { return };
        match item {
            Item::Open => open_main(),
            Item::Power => send(hwnd, Tag::Plain, Request::SetEnabled { enabled: paused }),
            Item::Learn if learning => send(hwnd, Tag::Plain, Request::StopLearning),
            Item::Learn => send(hwnd, Tag::Plain, Request::Relearn),
            Item::Update if update.state == UpdateState::Available => send(hwnd, Tag::Plain, Request::InstallUpdate),
            Item::Update => send(hwnd, Tag::Plain, Request::CheckUpdate),
            Item::Autostart => toggle_autostart(hwnd, !autostart),
            Item::Quit => exit(),
        }
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // The whole window is client area: no system frame, but DWM still gives shadow and rounded corners.
            WM_NCCALCSIZE => LRESULT(0),
            WM_NCHITTEST => {
                let (sx, sy) = xy(lparam);
                let mut p = POINT { x: sx, y: sy };
                let _ = ScreenToClient(hwnd, &mut p);
                let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
                let (x, y) = (p.x as f32 / scale, p.y as f32 / scale);
                let caption = with(|app| app.main.as_ref().is_some_and(|m| m.ui.caption(x, y))).unwrap_or(y < ui::BAR);
                if caption { LRESULT(HTCAPTION as isize) } else { LRESULT(HTCLIENT as isize) }
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                with(paint);
                let _ = ValidateRect(Some(hwnd), None);
                LRESULT(0)
            }
            WM_ACTIVATE => {
                let focused = loword(wparam.0) != WA_INACTIVE;
                with(|app| {
                    if let Some(main) = &mut app.main {
                        main.ui.focused = focused;
                        invalidate(main);
                    }
                });
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_SIZE => {
                let (w, h) = ((lparam.0 & 0xffff) as u32, ((lparam.0 >> 16) & 0xffff) as u32);
                with(|app| {
                    if let Some(main) = &mut app.main {
                        if let Some(canvas) = &mut main.canvas {
                            canvas.resize(w, h, main.dpi as f32);
                        }
                    }
                });
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = loword(wparam.0);
                with(|app| {
                    if let Some(main) = &mut app.main {
                        main.dpi = dpi;
                        main.canvas = None;
                        main.ui.drop_field();
                    }
                });
                let r = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (px, py) = xy(lparam);
                let track = with(|app| {
                    let main = app.main.as_mut()?;
                    let scale = main.dpi as f32 / 96.0;
                    let hit = main.ui.hit(px as f32 / scale, py as f32 / scale);
                    if hit != main.ui.hover {
                        main.ui.hover = hit;
                        invalidate(main);
                    }
                    let first = !main.tracking;
                    main.tracking = true;
                    Some(first)
                })
                .flatten();
                if track == Some(true) {
                    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with(|app| {
                    if let Some(main) = &mut app.main {
                        main.tracking = false;
                        main.ui.hover = None;
                        main.ui.press = None;
                        invalidate(main);
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (px, py) = xy(lparam);
                with(|app| {
                    if let Some(main) = &mut app.main {
                        let scale = main.dpi as f32 / 96.0;
                        main.ui.hover = main.ui.hit(px as f32 / scale, py as f32 / scale);
                        main.ui.press = main.ui.hover;
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (px, py) = xy(lparam);
                let next = with(|app| {
                    let main = app.main.as_mut()?;
                    let scale = main.dpi as f32 / 96.0;
                    main.ui.hover = main.ui.hit(px as f32 / scale, py as f32 / scale);
                    let (hover, press) = (main.ui.hover, main.ui.press.take());
                    let hit = hover.filter(|h| Some(*h) == press)?;
                    let shared = app.store.lock().unwrap_or_else(|e| e.into_inner());
                    let todo = main.ui.click(hit, shared.status.as_ref().filter(|_| shared.connected));
                    drop(shared);
                    invalidate(main);
                    act(app, todo)
                })
                .flatten();
                after(hwnd, next);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
                with(|app| {
                    if let Some(main) = &mut app.main {
                        if main.ui.wheel(delta) {
                            invalidate(main);
                        }
                    }
                });
                LRESULT(0)
            }
            WM_SETCURSOR if loword(lparam.0 as usize) == HTCLIENT => {
                let hand = with(|app| app.main.as_ref().is_some_and(|m| m.ui.hover.is_some())).unwrap_or(false);
                SetCursor(LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).ok());
                LRESULT(1)
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
                let next = with(|app| {
                    let main = app.main.as_mut()?;
                    if main.ui.screen == Screen::Home {
                        return Some(Act::Close);
                    }
                    let todo = main.ui.go(Screen::Home);
                    invalidate(main);
                    act(app, todo)
                })
                .flatten();
                after(hwnd, next);
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                // Everything the window owned goes away with it: render target, field, fonts.
                with(|app| app.main = None);
                trim();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
