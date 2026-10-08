//! Embed the app icon and version resource into the Windows executable.
//!
//! Works for native MSVC builds (rc.exe from the VS Build Tools) and for
//! Linux cross-builds (windres from mingw-w64 — this is what CI uses).
//! When no resource compiler can be found (e.g. a bare
//! `cargo check --target x86_64-pc-windows-msvc` on a Linux dev box) it
//! prints a warning and skips, so plain type-checking still works.

use std::path::Path;
use std::process::{Command, Stdio};

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let icon = Path::new("assets/pound.ico");

    if target_os == "windows" && icon.exists() {
        if cfg!(windows) {
            compile(None);
        } else if let Some(windres) = find_windres() {
            compile(Some(windres));
        } else {
            println!(
                "cargo:warning=no windres found - building without the embedded icon \
                 (install mingw-w64 to embed it)"
            );
        }
    }

    println!("cargo:rerun-if-changed=assets/pound.ico");
    println!("cargo:rerun-if-changed=build.rs");
}

fn compile(windres: Option<String>) {
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/pound.ico");
    resource.set("FileDescription", "Pound — markdown reader");
    resource.set("LegalCopyright", "MIT License");
    if let Some(windres) = windres {
        resource.set_windres_path(&windres);
    }
    resource
        .compile()
        .expect("failed to compile Windows resources (is rc.exe / windres installed?)");
}

/// Locate a usable windres for cross-builds: $WINDRES, then the mingw-w64
/// target-prefixed name, then a generic `windres`.
fn find_windres() -> Option<String> {
    if let Ok(path) = std::env::var("WINDRES") {
        return Some(path);
    }
    for tool in ["x86_64-w64-mingw32-windres", "windres"] {
        let runnable = Command::new(tool)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if runnable {
            return Some(tool.to_owned());
        }
    }
    None
}
