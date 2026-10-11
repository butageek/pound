//! Installed system fonts for the Settings page.
//!
//! The host enumerates the families once at startup (GDI — this is what
//! every Win32 font picker uses, and unlike reading the registry it also
//! sees per-user fonts) and injects them into the shell as
//! `window.poundFonts`; the settings select then offers exactly what is
//! installed, so the list can never grow stale or mention fonts this
//! machine does not have.

#[cfg(windows)]
pub fn families() -> Vec<String> {
    use std::collections::BTreeSet;
    use windows_sys::Win32::Graphics::Gdi::{
        EnumFontFamiliesExW, GetDC, ReleaseDC, DEFAULT_CHARSET, LOGFONTW, TEXTMETRICW,
    };

    /// GDI calls this once per (family, charset) it finds; we keep one
    /// entry per family. Faces starting with `@` are GDI's
    /// vertical-writing variants — real picks, never wanted.
    unsafe extern "system" fn collect(
        logfont: *const LOGFONTW,
        _metric: *const TEXTMETRICW,
        _font_type: u32,
        lparam: isize,
    ) -> i32 {
        let seen = &mut *(lparam as *mut BTreeSet<String>);
        let raw = &(*logfont).lfFaceName;
        let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        let name = String::from_utf16_lossy(&raw[..len]);
        if !name.is_empty() && !name.starts_with('@') {
            seen.insert(name);
        }
        1 // continue enumeration
    }

    // BTreeSet keeps the list sorted and deduplicated.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if !hdc.is_null() {
            // An empty face name + DEFAULT_CHARSET enumerates one entry
            // per installed family across all scripts.
            let mut logfont: LOGFONTW = std::mem::zeroed();
            logfont.lfCharSet = DEFAULT_CHARSET;
            EnumFontFamiliesExW(
                hdc,
                &logfont,
                Some(collect),
                &mut seen as *mut _ as isize,
                0,
            );
            ReleaseDC(std::ptr::null_mut(), hdc);
        }
    }
    seen.into_iter().collect()
}

/// Non-Windows hosts never open the GUI; the shell handles an empty list
/// by showing just the "System default" entry.
#[cfg(not(windows))]
pub fn families() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The stub is what non-Windows hosts see; on Windows the real
    // enumeration returns the machine's fonts (and needs a desktop
    // session), so this only runs off-Windows.
    #[cfg(not(windows))]
    #[test]
    fn non_windows_list_is_empty() {
        assert!(families().is_empty());
    }
}
