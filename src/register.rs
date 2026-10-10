//! Windows integration (Windows-only).
//!
//! Registers Pound so it shows up in the system's app list ("Default apps")
//! and can be chosen as the handler for `.md` files. Everything is written
//! under `HKEY_CURRENT_USER`, so no administrator rights are required.
//!
//! Keys created by `register`:
//!
//! ```text
//! HKCU\Software\Classes\Pound.md                      ProgId (description)
//!     \DefaultIcon                                    -> "pound.exe",0
//!     \shell\open\command                             -> "pound.exe" "%1"
//! HKCU\Software\Classes\.md\OpenWithProgids           -> Pound.md
//!     (and the .md default value when --default is passed)
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.md\OpenWithProgids
//! HKCU\Software\RegisteredApplications   Pound = Software\Pound\Capabilities
//! HKCU\Software\Pound\Capabilities
//!     ApplicationName / ApplicationDescription
//!     FileAssociations\.md = Pound.md
//! ```

use crate::theme::{DarkPalette, LightPalette, Settings, Theme};
use std::io;
use std::path::PathBuf;

use winreg::enums::*;
use winreg::RegKey;

const PROG_ID: &str = "Pound.md";
const APP_NAME: &str = "Pound";
const APP_DESCRIPTION: &str = "Markdown reader with a side-by-side source pane";

fn exe_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot locate the pound executable: {e}"))
}

fn quoted(path: &PathBuf) -> String {
    format!("\"{}\"", path.display())
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

/// All appearance settings. Garbage values (e.g. written by a future
/// version) fall back to the defaults per-field.
pub fn load_settings() -> Settings {
    let get = |name: &str| load_setting(name).unwrap_or_default();
    Settings {
        theme: Theme::parse(&get("Theme")).unwrap_or_default(),
        light: LightPalette::parse(&get("LightPalette")).unwrap_or_default(),
        dark: DarkPalette::parse(&get("DarkPalette")).unwrap_or_default(),
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

    // --- ProgId ---------------------------------------------------------
    let classes = hkcu
        .create_subkey("Software\\Classes")
        .map_err(|e| ctx("open HKCU\\Software\\Classes", e))?
        .0;

    let (prog_id, _) = classes
        .create_subkey(PROG_ID)
        .map_err(|e| ctx("create ProgId", e))?;
    prog_id
        .set_value("", &format!("{APP_NAME} Markdown Document"))
        .map_err(|e| ctx("describe ProgId", e))?;

    let (icon, _) = prog_id
        .create_subkey("DefaultIcon")
        .map_err(|e| ctx("create DefaultIcon", e))?;
    icon.set_value("", &format!("{},0", quoted(&exe)))
        .map_err(|e| ctx("set icon", e))?;

    let (open, _) = prog_id
        .create_subkey("shell\\open\\command")
        .map_err(|e| ctx("create open command", e))?;
    open.set_value("", &format!("{} \"%1\"", quoted(&exe)))
        .map_err(|e| ctx("set open command", e))?;

    // --- .md association -------------------------------------------------
    let (md, _) = classes
        .create_subkey(".md")
        .map_err(|e| ctx("open .md class", e))?;
    let (open_with, _) = md
        .create_subkey("OpenWithProgids")
        .map_err(|e| ctx("create OpenWithProgids", e))?;
    open_with
        .set_value(PROG_ID, &"")
        .map_err(|e| ctx("register in OpenWithProgids", e))?;

    let (exts, _) = hkcu
        .create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.md\\OpenWithProgids")
        .map_err(|e| ctx("open FileExts\\.md", e))?;
    exts.set_value(PROG_ID, &"")
        .map_err(|e| ctx("register in FileExts", e))?;

    if set_default {
        md.set_value("", &PROG_ID)
            .map_err(|e| ctx("set .md default handler", e))?;
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

    let (assoc, _) = caps
        .create_subkey("FileAssociations")
        .map_err(|e| ctx("create FileAssociations", e))?;
    assoc
        .set_value(".md", &PROG_ID)
        .map_err(|e| ctx("associate .md", e))?;

    notify_assoc_changed();

    if set_default {
        println!("{APP_NAME} registered and set as the handler for .md files.");
        println!("If Windows still asks which app to use, confirm once with \"Always\" or");
        println!("run:  start ms-settings:defaultapps");
    } else {
        println!("{APP_NAME} registered in the app list; use \"Open with > Choose another app\"");
        println!("to pick it for .md files, or re-run with --default.");
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

    // ProgId tree.
    if let Ok(classes) = hkcu.open_subkey_with_flags("Software\\Classes", KEY_READ) {
        delete_tree(&classes, PROG_ID);

        // .md cleanup: remove ourselves from OpenWithProgids; if we were the
        // default handler, clear it so Windows falls back to its own logic.
        if let Ok(md) = classes.open_subkey_with_flags(".md", KEY_READ | KEY_SET_VALUE) {
            if let Ok(owp) = md.open_subkey_with_flags("OpenWithProgids", KEY_SET_VALUE) {
                let _ = owp.delete_value(PROG_ID);
            }
            if let Ok(current) = md.get_value::<String, _>("") {
                if current == PROG_ID {
                    let _ = md.delete_value("");
                }
            }
        }
    }

    if let Ok(exts) = hkcu.open_subkey_with_flags(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.md\\OpenWithProgids",
        KEY_SET_VALUE,
    ) {
        let _ = exts.delete_value(PROG_ID);
    }

    notify_assoc_changed();
    println!("{APP_NAME} unregistered.");
    Ok(())
}
