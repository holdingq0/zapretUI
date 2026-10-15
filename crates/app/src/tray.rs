use rkn_core::ipc::State;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject, HGDIOBJ};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO};

use crate::pipe::Shared;

pub const MESSAGE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;
const SIZE: u32 = 32;

const OK: [u8; 3] = [0x7c, 0xe3, 0x8b];
const MUTE: [u8; 3] = [0x8a, 0x8a, 0x8a];
const BAD: [u8; 3] = [0xff, 0x7a, 0x70];

/// The dot-matrix mark in the colour of the state, premultiplied BGRA for CreateIconIndirect.
fn draw(color: Option<[u8; 3]>, dim: bool, light_taskbar: bool) -> Vec<u8> {
    let plain: [u8; 3] = if light_taskbar { [0x1b, 0x1b, 0x1b] } else { [0xff, 0xff, 0xff] };
    let mut bgra = crate::pixel::render(SIZE as usize, color.unwrap_or(plain), if dim { 0.55 } else { 1.0 }, None);
    for px in bgra.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        for c in &mut px[..3] {
            *c = (u32::from(*c) * a / 255) as u8;
        }
    }
    bgra
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub dot: Option<[u8; 3]>,
    pub dim: bool,
    pub label: &'static str,
    pub light: bool,
}

pub fn look(shared: &Shared) -> Look {
    let light = crate::autostart::light_taskbar();
    let Some(status) = shared.status.as_ref().filter(|_| shared.connected) else {
        return Look { dot: Some(MUTE), dim: true, label: "служба не отвечает", light };
    };
    let (dot, label) = match status.state {
        State::Working => (Some(OK), "обход работает"),
        State::Paused => (Some(MUTE), "обход на паузе"),
        State::Starting => (None, "запуск"),
        State::Learning => (None, "подбираю способ обхода"),
        State::Tuning => (None, "нужна настройка"),
        State::Whitelist => (Some(BAD), "белый список оператора"),
        State::EngineMissing | State::Failed => (Some(BAD), "проблема с движком"),
    };
    Look { dot, dim: false, label, light }
}

pub fn icon(look: &Look) -> HICON {
    unsafe {
        let bgra = draw(look.dot, look.dim, look.light);
        let color = CreateBitmap(SIZE as i32, SIZE as i32, 1, 32, Some(bgra.as_ptr().cast()));
        let mask_bits = vec![0u8; (SIZE * SIZE / 8) as usize];
        let mask = CreateBitmap(SIZE as i32, SIZE as i32, 1, 1, Some(mask_bits.as_ptr().cast()));
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}

fn copy(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    shown: Option<Look>,
}

impl Tray {
    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.hwnd, uID: 1, ..Default::default() }
    }

    pub fn new(hwnd: HWND) -> Self {
        Self { hwnd, icon: HICON::default(), shown: None }
    }

    /// (Re)creates the icon, e.g. at start and after Explorer restarts.
    pub fn add(&mut self, look: Look) {
        unsafe {
            let mut d = self.data();
            d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            d.uCallbackMessage = MESSAGE;
            self.replace_icon(&look);
            d.hIcon = self.icon;
            copy(&mut d.szTip, &format!("zapretUI — {}", look.label));
            let _ = Shell_NotifyIconW(NIM_ADD, &d);
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
        self.shown = Some(look);
    }

    fn replace_icon(&mut self, look: &Look) {
        let old = self.icon;
        self.icon = icon(look);
        if !old.is_invalid() {
            unsafe {
                let _ = DestroyIcon(old);
            }
        }
    }

    /// Touches the shell only when the icon really changes.
    pub fn update(&mut self, look: Look) {
        if self.shown == Some(look) {
            return;
        }
        self.replace_icon(&look);
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_TIP;
        d.hIcon = self.icon;
        copy(&mut d.szTip, &format!("zapretUI — {}", look.label));
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
        self.shown = Some(look);
    }

}

impl Drop for Tray {
    fn drop(&mut self) {
        let d = self.data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            if !self.icon.is_invalid() {
                let _ = DestroyIcon(self.icon);
            }
        }
    }
}
