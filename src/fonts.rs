//! Installed system fonts for the Settings page.
//!
//! The host enumerates the families once at startup (GDI — this is what
//! every Win32 font picker uses, and unlike reading the registry it also
//! sees per-user fonts) and injects them into the shell as
//! `window.poundFonts`; the settings selects then offer exactly what is
//! installed, so the list can never grow stale or mention fonts this
//! machine does not have.

/// GDI's pitch bit (`NEWTEXTMETRIC.tmPitchAndFamily`). Quirk: the bit
/// named FIXED_PITCH is set on *variable*-pitch fonts — fixed pitch is
/// the cleared bit (documented as the opposite of the constant's name).
const TMPF_FIXED_PITCH: u32 = 0x01;

/// The installed font families, sorted and deduplicated.
#[derive(Debug, Default, Clone)]
pub struct FontFamilies {
    /// Every family on the system (the reading-font list).
    pub all: Vec<String>,
    /// Families with a fixed-pitch representative face (the editor list).
    pub mono: Vec<String>,
}

/// Whether a `tmPitchAndFamily` value marks a fixed-pitch (monospace)
/// face. Pure helper so the inverted GDI bit is tested headlessly.
fn is_monospace(pitch_and_family: u32) -> bool {
    pitch_and_family & TMPF_FIXED_PITCH == 0
}

#[cfg(windows)]
pub fn families() -> FontFamilies {
    use std::collections::BTreeMap;
    use windows_sys::Win32::Graphics::Gdi::{
        EnumFontFamiliesExW, GetDC, ReleaseDC, DEFAULT_CHARSET, LOGFONTW,
    };

    /// GDI calls this once per (family, charset) it finds; we fold the
    /// faces into one entry per family, remembering whether any face of
    /// the family is fixed-pitch. Faces starting with `@` are GDI's
    /// vertical-writing variants — real picks, never wanted.
    unsafe extern "system" fn collect(
        logfont: *const LOGFONTW,
        metric: *const windows_sys::Win32::Graphics::Gdi::TEXTMETRICW,
        _font_type: u32,
        lparam: isize,
    ) -> i32 {
        let seen = &mut *(lparam as *mut BTreeMap<String, bool>);
        let raw = &(*logfont).lfFaceName;
        let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        let name = String::from_utf16_lossy(&raw[..len]);
        if !name.is_empty() && !name.starts_with('@') {
            let mono = is_monospace((*metric).tmPitchAndFamily as u32);
            let entry = seen.entry(name).or_insert(false);
            if mono {
                *entry = true;
            }
        }
        1 // continue enumeration
    }

    // family -> any fixed-pitch face seen (BTreeMap keeps it sorted)
    let mut seen: BTreeMap<String, bool> = BTreeMap::new();
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
    FontFamilies {
        all: seen.keys().cloned().collect(),
        mono: seen
            .iter()
            .filter(|(_, mono)| **mono)
            .map(|(name, _)| name.clone())
            .collect(),
    }
}

/// Non-Windows hosts never open the GUI; the shell handles empty lists
/// by showing just the "System default" entry.
#[cfg(not(windows))]
pub fn families() -> FontFamilies {
    FontFamilies::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_pitch_is_the_cleared_bit() {
        // GDI's constant is inverted: the bit set = variable pitch.
        assert!(is_monospace(0));
        assert!(!is_monospace(TMPF_FIXED_PITCH));
        // TrueType monospace faces report TMPF_TRUETYPE only.
        assert!(is_monospace(0x04));
    }

    #[test]
    fn non_windows_lists_are_empty() {
        let fonts = families();
        assert!(fonts.all.is_empty());
        assert!(fonts.mono.is_empty());
    }
}
