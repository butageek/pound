//! Pound — a small markdown reader with a side-by-side source pane.
//!
//! Architecture (MVP pattern):
//! - [`model`]      : state (document, view mode, errors)
//! - [`markdown`]   : parsing (markdown source -> neutral block tree)
//! - [`presenter`]  : user intents / use-cases over the model
//! - [`view`]       : egui/eframe rendering; forwards events to the presenter

// Hide the console window when double-clicking a .md file on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod markdown;
mod model;
mod presenter;
#[cfg(windows)]
mod register;
mod view;

use std::path::PathBuf;

use cli::Command;
use eframe::egui;

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

fn run_gui(file: Option<PathBuf>) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([560.0, 400.0])
            .with_title("Pound"),
        ..Default::default()
    };
    let creator: eframe::AppCreator =
        Box::new(|cc| Ok(Box::new(view::AppView::new(cc, file)) as Box<dyn eframe::App>));
    if let Err(e) = eframe::run_native("pound", options, creator) {
        eprintln!("pound: {e}");
        std::process::exit(1);
    }
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
