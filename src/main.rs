//! Pound — a small markdown reader with a side-by-side source pane.
//!
//! Architecture (MVP pattern):
//! - [`model`]      : state (document, view mode, errors)
//! - [`markdown`]   : rendering input (markdown source -> sanitized HTML)
//! - [`presenter`]  : user intents / use-cases over the model
//! - [`view`]       : WebView2 shell (tao + wry) showing the HTML with
//!   VSCode-grade browser rendering

// Hide the console window when double-clicking a .md file on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
// The GUI core is exercised by the Windows view and the test suite;
// non-Windows hosts run the tests only.
#[cfg(any(windows, test))]
mod markdown;
#[cfg(any(windows, test))]
mod model;
#[cfg(any(windows, test))]
mod presenter;
#[cfg(windows)]
mod register;
#[cfg(any(windows, test))]
mod theme;
#[cfg(any(windows, test))]
mod update;
#[cfg(windows)]
mod view;

use std::path::PathBuf;

use cli::Command;

fn main() {
    match cli::parse(std::env::args().skip(1)) {
        Command::Gui { file } => run_gui(file),
        Command::Register { set_default } => {
            prepare_console();
            exit_on_err(register_impl(set_default));
        }
        Command::Unregister => {
            prepare_console();
            exit_on_err(unregister_impl());
        }
        Command::Help => {
            prepare_console();
            println!("{}", cli::usage());
        }
        Command::Version => {
            prepare_console();
            println!("pound {}", env!("CARGO_PKG_VERSION"));
        }
    }
}

#[cfg(windows)]
fn run_gui(file: Option<PathBuf>) {
    view::run(file);
}

#[cfg(not(windows))]
fn run_gui(_file: Option<PathBuf>) {
    // The GUI targets Windows (WebView2). Other platforms run the unit
    // tests only.
    eprintln!("pound: the GUI is built for Windows; this platform runs `cargo test` only.");
}

#[cfg(windows)]
fn prepare_console() {
    register::attach_parent_console();
}

#[cfg(not(windows))]
fn prepare_console() {}

#[cfg(windows)]
fn register_impl(set_default: bool) -> Result<(), String> {
    register::register(set_default)
}

#[cfg(windows)]
fn unregister_impl() -> Result<(), String> {
    register::unregister()
}

#[cfg(not(windows))]
fn register_impl(_: bool) -> Result<(), String> {
    Err("registration is only supported on Windows".to_string())
}

#[cfg(not(windows))]
fn unregister_impl() -> Result<(), String> {
    Err("registration is only supported on Windows".to_string())
}

fn exit_on_err(result: Result<(), String>) {
    if let Err(e) = result {
        eprintln!("pound: {e}");
        std::process::exit(1);
    }
}
