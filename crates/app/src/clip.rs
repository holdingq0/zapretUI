// The text on the clipboard: the window has no text fields, an address is brought in by copying it.

use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};

const CF_UNICODETEXT: u32 = 13;
/// An address is short; anything longer on the clipboard is not one.
const MAX_CHARS: usize = 400;

pub fn text() -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT).ok().and_then(|handle| {
            let memory = HGLOBAL(handle.0);
            let start = GlobalLock(memory) as *const u16;
            if start.is_null() {
                return None;
            }
            // Clipboard text ends with a zero.
            let mut len = 0;
            while len <= MAX_CHARS && *start.add(len) != 0 {
                len += 1;
            }
            let text = (len <= MAX_CHARS).then(|| String::from_utf16_lossy(std::slice::from_raw_parts(start, len)));
            let _ = GlobalUnlock(memory);
            text
        });
        let _ = CloseClipboard();
        text
    }
}
