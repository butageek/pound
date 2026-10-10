//! Win32 clipboard read, for the editor's Paste action.
//!
//! WebView2 cannot read the clipboard from JavaScript without a
//! permission prompt, so the shell asks the host over ipc and the text
//! comes back through `pound.setClipboardText`. Writing (Copy/Cut)
//! stays on the JS side, where `execCommand` keeps it on the editor's
//! native undo stack.

#![allow(clippy::missing_safety_doc)] // trivially safe: no invariants on our side

use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};

const CF_UNICODETEXT: u32 = 13;

/// The clipboard as text, or `None` when it holds no text (or is busy).
pub fn read_text() -> Option<String> {
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let text = clipboard_unicode_text();
        CloseClipboard();
        text
    }
}

/// Caller must hold the clipboard open.
unsafe fn clipboard_unicode_text() -> Option<String> {
    let handle = GetClipboardData(CF_UNICODETEXT);
    if handle.is_null() {
        return None; // not text (image, files, …)
    }
    let wide = GlobalLock(handle) as *const u16;
    if wide.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *wide.add(len) != 0 {
        len += 1;
    }
    let text = String::from_utf16_lossy(std::slice::from_raw_parts(wide, len));
    GlobalUnlock(handle);
    Some(text)
}
