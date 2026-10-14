//! Windows integration (Windows-only).
//!
//! Registers Pound so it shows up in the system's app list ("Default
//! apps") and can be chosen as the handler for every file kind it
//! opens. Everything is written under `HKEY_CURRENT_USER`, so no
//! administrator rights are required.
//!
//! Keys created by `register`:
//!
//! ```text
//! HKCU\Software\Classes\Pound.md / Pound.table / Pound.text   ProgIds
//!     \DefaultIcon                                    -> "pound.exe",0
//!     \shell\open\command                             -> "pound.exe" "%1"
//! HKCU\Software\Classes\<ext>\OpenWithProgids        -> the ProgId
//!     (and the .md default value when --default is passed)
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\<ext>\OpenWithProgids
//! HKCU\Software\RegisteredApplications   Pound = Software\Pound\Capabilities
//! HKCU\Software\Pound\Capabilities
//!     ApplicationName / ApplicationDescription
//!     FileAssociations\<ext> = the ProgId
//! ```

use crate::theme::{
    parse_font_size, DarkPalette, LightPalette, Settings, Theme, DEFAULT_FONT_SIZE,
};
use std::io;
use std::path::PathBuf;

use winreg::enums::*;
use winreg::RegKey;

/// Every extension Pound registers for, mapped to its ProgId (the
/// ProgIds' own history lives below). Any text file *opens* (the
/// editor pane is its own source); the plain-text group is the curated
/// set we offer in Explorer's lists — common text kinds people open in
/// a viewer, none claimed as a default (the user picks per type).
const ASSOCIATIONS: &[(&str, &str)] = &[
    (".md", PROG_ID),
    (".csv", TABLE_PROG_ID),
    (".tsv", TABLE_PROG_ID),
    (".txt", TEXT_PROG_ID),
    (".log", TEXT_PROG_ID),
    (".ini", TEXT_PROG_ID),
    (".cfg", TEXT_PROG_ID),
    (".conf", TEXT_PROG_ID),
    (".json", TEXT_PROG_ID),
    (".xml", TEXT_PROG_ID),
    (".yaml", TEXT_PROG_ID),
    (".yml", TEXT_PROG_ID),
    (".toml", TEXT_PROG_ID),
];

const PROG_ID: &str = "Pound.md";
const TABLE_PROG_ID: &str = "Pound.table";
const TEXT_PROG_ID: &str = "Pound.text";
const APP_NAME: &str = "Pound";
const APP_DESCRIPTION: &str = "Viewer for markdown, CSV and text files";

/// The ProgIds Pound writes, with their Explorer-facing descriptions.
/// `Pound.md` predates the table views (installed users already carry
/// it, and `--default` still keys off it); the data formats share one,
/// and so does the plain-text set.
const PROG_IDS: &[(&str, &str)] = &[
    (PROG_ID, "Pound Markdown Document"),
    (TABLE_PROG_ID, "Pound Data Table"),
    (TEXT_PROG_ID, "Pound Text File"),
];

fn exe_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot locate the pound executable: {e}"))
}

fn quoted(path: &PathBuf) -> String {
    format!("\"{}\"", path.display())
}

/// println!, but tolerant of a closed stdout: installers capture this
/// output via redirected pipes and sometimes close them early, and a
/// print panic would fail an otherwise successful registration.
/// (`writeln!` returns an error instead of panicking; errors ignored.)
fn say(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn ctx(what: &str, e: io::Error) -> String {
    format!("registry operation failed ({what}): {e}")
}

/// Recursively delete a subkey tree, ignoring "not found".
fn delete_tree(parent: &RegKey, name: &str) {
    if let Ok(child) = parent.open_subkey_with_flags(name, KEY_READ) {
        let subkeys: Vec<String> = child.enum_keys().filter_map(Result::ok).collect();
        for sub in subkeys {
            delete_tree(&child, &sub);
        }
    }
    let _ = parent.delete_subkey(name);
}

/// Tell Explorer the file-association world changed.
fn notify_assoc_changed() {
    use windows_sys::Win32::UI::Shell::{
        SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_FLUSH, SHCNF_IDLIST,
    };
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST | SHCNF_FLUSH,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
}

/// Attach to the parent console so CLI output is visible when the exe (a
/// windowed-subsystem binary) is launched from PowerShell/cmd.
///
/// Only attaches when we have no usable stdout handle. If stdout is
/// already a console, pipe, or redirected file (installers capture output
/// via `Start-Process -RedirectStandardOutput`), we leave it alone so the
/// redirection keeps working.
/// Read one setting from `HKCU\Software\Pound`; `None` when missing or
/// unreadable (callers fall back to defaults).
fn load_setting(name: &str) -> Option<String> {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Pound")
        .and_then(|key| key.get_value::<String, _>(name))
        .ok()
}

/// Persist one setting. Best-effort: a locked-down registry just means
/// the choice lasts for this session.
pub fn save_setting(name: &str, value: &str) {
    let written = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey("Software\\Pound")
        .and_then(|(key, _)| key.set_value(name, &value));
    if let Err(e) = written {
        eprintln!("pound: could not save the {name} setting: {e}");
    }
}

/// Whether Windows is in dark app mode — the Rust-side answer to the
/// shell's `prefers-color-scheme` matchMedia, used to pick the
/// pre-paint background while Auto. False when the key is missing
/// (old Windows: light is the historical default).
pub fn system_prefers_dark() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
        .and_then(|key| key.get_value::<u32, _>("AppsUseLightTheme"))
        .is_ok_and(|apps_use_light| apps_use_light == 0)
}

/// The last window size in physical pixels, saved by the view on every
/// resize (not while maximized). `None` when missing or nonsensical —
/// a broken value must never produce an unusable window (the mins
/// mirror the builder's `with_min_inner_size`).
pub fn window_size() -> Option<(u32, u32)> {
    let width = load_setting("WindowWidth")?.parse::<u32>().ok()?;
    let height = load_setting("WindowHeight")?.parse::<u32>().ok()?;
    let sane = (560..=16384).contains(&width) && (400..=16384).contains(&height);
    sane.then_some((width, height))
}

/// All appearance settings. Garbage values (e.g. written by a future
/// version) fall back to the defaults per-field.
pub fn load_settings() -> Settings {
    let get = |name: &str| load_setting(name).unwrap_or_default();
    // Font settings are free-form names; empty means "default". The
    // single Font value is absent until first saved after v0.10.0 (two
    // separate settings then) — fall back to whichever old value was
    // set so an upgrade keeps the user's choice.
    let font = match load_setting("Font") {
        Some(value) if !value.trim().is_empty() => Some(value),
        Some(_) => None, // saved "": an explicit System default
        None => load_setting("ReaderFont")
            .filter(|v| !v.trim().is_empty())
            .or_else(|| load_setting("MonoFont").filter(|v| !v.trim().is_empty())),
    };
    Settings {
        theme: Theme::parse(&get("Theme")).unwrap_or_default(),
        light: LightPalette::parse(&get("LightPalette")).unwrap_or_default(),
        dark: DarkPalette::parse(&get("DarkPalette")).unwrap_or_default(),
        font_size: parse_font_size(&get("FontSize"))
            // unwrap_or_default() would yield f32's 0.0 — invisible text,
            // the v0.11.0 bug — so a missing/garbage value means default.
            .unwrap_or(DEFAULT_FONT_SIZE),
        font,
    }
}

pub fn attach_parent_console() {
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_UNKNOWN};
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if GetFileType(stdout) == FILE_TYPE_UNKNOWN {
            // No inherited/redirected stdout (e.g. launched from Explorer,
            // or by a shell that gives GUI-subsystem processes no handles).
            // Attach to the parent's console, if it has one.
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

pub fn register(set_default: bool) -> Result<(), String> {
    let exe = exe_path()?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // --- ProgIds ---------------------------------------------------------
    let classes = hkcu
        .create_subkey("Software\\Classes")
        .map_err(|e| ctx("open HKCU\\Software\\Classes", e))?
        .0;

    for (prog_id, description) in PROG_IDS {
        let (key, _) = classes
            .create_subkey(prog_id)
            .map_err(|e| ctx("create ProgId", e))?;
        key.set_value("", description)
            .map_err(|e| ctx("describe ProgId", e))?;

        let (icon, _) = key
            .create_subkey("DefaultIcon")
            .map_err(|e| ctx("create DefaultIcon", e))?;
        icon.set_value("", &format!("{},0", quoted(&exe)))
            .map_err(|e| ctx("set icon", e))?;

        let (open, _) = key
            .create_subkey("shell\\open\\command")
            .map_err(|e| ctx("create open command", e))?;
        open.set_value("", &format!("{} \"%1\"", quoted(&exe)))
            .map_err(|e| ctx("set open command", e))?;
    }

    // --- Extension associations -------------------------------------------
    for (ext, prog_id) in ASSOCIATIONS {
        let (class, _) = classes
            .create_subkey(ext)
            .map_err(|e| ctx("open extension class", e))?;
        let (open_with, _) = class
            .create_subkey("OpenWithProgids")
            .map_err(|e| ctx("create OpenWithProgids", e))?;
        open_with
            .set_value(prog_id, &"")
            .map_err(|e| ctx("register in OpenWithProgids", e))?;

        let (exts, _) = hkcu
            .create_subkey(format!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\{ext}\\OpenWithProgids"
            ))
            .map_err(|e| ctx("open FileExts entry", e))?;
        exts.set_value(prog_id, &"")
            .map_err(|e| ctx("register in FileExts", e))?;

        let (assoc, _) = hkcu
            .create_subkey("Software\\Pound\\Capabilities\\FileAssociations")
            .map_err(|e| ctx("create FileAssociations", e))?;
        assoc
            .set_value(ext, prog_id)
            .map_err(|e| ctx("associate extension", e))?;
    }

    // --- RegisteredApplications + Capabilities -----------------------------
    let (registered, _) = hkcu
        .create_subkey("Software\\RegisteredApplications")
        .map_err(|e| ctx("open RegisteredApplications", e))?;
    registered
        .set_value(APP_NAME, &"Software\\Pound\\Capabilities")
        .map_err(|e| ctx("register application", e))?;

    let (caps, _) = hkcu
        .create_subkey("Software\\Pound\\Capabilities")
        .map_err(|e| ctx("create Capabilities", e))?;
    caps.set_value("ApplicationName", &APP_NAME)
        .map_err(|e| ctx("set application name", e))?;
    caps.set_value("ApplicationDescription", &APP_DESCRIPTION)
        .map_err(|e| ctx("set application description", e))?;

    if set_default {
        // Only .md: claiming the data formats' defaults from a flag would
        // decide a preference the user never expressed.
        let (md, _) = classes
            .create_subkey(".md")
            .map_err(|e| ctx("open .md class", e))?;
        md.set_value("", &PROG_ID)
            .map_err(|e| ctx("set .md default handler", e))?;
    }

    notify_assoc_changed();

    if set_default {
        say(&format!(
            "{APP_NAME} registered and set as the handler for .md files."
        ));
        say("If Windows still asks which app to use, confirm once with \"Always\" or");
        say("run:  start ms-settings:defaultapps");
    } else {
        say(&format!(
            "{APP_NAME} registered in the app list for markdown, data and text files;"
        ));
        say("use \"Open with > Choose another app\" to pick it for any of them.");
    }
    Ok(())
}

pub fn unregister() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // Capabilities tree + RegisteredApplications entry.
    let software = hkcu
        .open_subkey_with_flags("Software", KEY_READ)
        .map_err(|e| ctx("open HKCU\\Software", e))?;
    delete_tree(&software, "Pound");
    if let Ok(registered) =
        hkcu.open_subkey_with_flags("Software\\RegisteredApplications", KEY_SET_VALUE)
    {
        let _ = registered.delete_value(APP_NAME);
    }

    // ProgId trees.
    if let Ok(classes) = hkcu.open_subkey_with_flags("Software\\Classes", KEY_READ) {
        for (prog_id, _) in PROG_IDS {
            delete_tree(&classes, prog_id);
        }

        // Extension cleanup: remove ourselves from OpenWithProgids; on
        // .md also clear the default when it was us (set via --default).
        for (ext, prog_id) in ASSOCIATIONS {
            let Ok(class) = classes.open_subkey_with_flags(ext, KEY_READ | KEY_SET_VALUE) else {
                continue;
            };
            if let Ok(owp) = class.open_subkey_with_flags("OpenWithProgids", KEY_SET_VALUE) {
                let _ = owp.delete_value(prog_id);
            }
            if *ext == ".md"
                && class
                    .get_value::<String, _>("")
                    .is_ok_and(|v| v == *prog_id)
            {
                let _ = class.delete_value("");
            }
        }
    }

    for (ext, prog_id) in ASSOCIATIONS {
        if let Ok(exts) = hkcu.open_subkey_with_flags(
            &format!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\{ext}\\OpenWithProgids"
            ),
            KEY_SET_VALUE,
        ) {
            let _ = exts.delete_value(prog_id);
        }
    }

    notify_assoc_changed();
    say(&format!("{APP_NAME} unregistered."));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The association table is pure data the registry loops consume —
    /// these invariants keep it well-formed (a duplicate extension
    /// would silently overwrite its own entry; an unknown ProgId would
    /// register an association nothing can open).
    #[test]
    fn associations_reference_known_prog_ids_without_duplicates() {
        for (ext, prog_id) in ASSOCIATIONS {
            assert!(
                ext.starts_with('.') && ext.len() > 1,
                "suspicious extension {ext}"
            );
            assert!(
                PROG_IDS.iter().any(|(id, _)| id == prog_id),
                "{ext} maps to unknown ProgId {prog_id}"
            );
        }
        // Duplicates would silently overwrite their own entry.
        let unique: std::collections::HashSet<_> =
            ASSOCIATIONS.iter().map(|(ext, _)| *ext).collect();
        assert_eq!(
            unique.len(),
            ASSOCIATIONS.len(),
            "duplicate extensions in ASSOCIATIONS"
        );
    }
}
