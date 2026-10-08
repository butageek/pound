//! Minimal hand-rolled CLI parsing (no external dependency).

use std::path::PathBuf;

#[derive(Debug)]
pub enum Command {
    /// Start the GUI, optionally opening this file.
    Gui {
        file: Option<PathBuf>,
    },
    /// Register Pound in the Windows app list / file association registry.
    Register {
        set_default: bool,
    },
    /// Remove all registry entries created by `register`.
    Unregister,
    Help,
    Version,
}

pub fn usage() -> String {
    format!(
        "pound {} — a small markdown reader

USAGE:
    pound [FILE]              open FILE (or start empty)
    pound register [--default]
                              register Pound as an app and associate .md files
                              --default also makes Pound the default .md handler
    pound unregister          remove all registry entries
    pound --help | --version

On Windows, double-clicking a .md file opens it once Pound is registered.
In the app, use the \"Source\" toggle for a side-by-side source view.",
        env!("CARGO_PKG_VERSION")
    )
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Command {
    let mut file: Option<PathBuf> = None;
    let mut mode: Option<Command> = None;
    let mut set_default = false;

    for arg in args {
        match arg.as_str() {
            "register" if mode.is_none() => mode = Some(Command::Register { set_default: false }),
            "unregister" if mode.is_none() => mode = Some(Command::Unregister),
            "--default" | "-d" => set_default = true,
            "--help" | "-h" | "help" => return Command::Help,
            "--version" | "-V" => return Command::Version,
            other if other.starts_with('-') => return Command::Help,
            other => {
                if file.is_none() {
                    file = Some(PathBuf::from(other));
                }
            }
        }
    }

    match mode {
        Some(Command::Register { .. }) => Command::Register { set_default },
        other => other.unwrap_or(Command::Gui { file }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn gui_with_file() {
        match parse(args(&["doc.md"])) {
            Command::Gui { file } => assert_eq!(file.unwrap(), PathBuf::from("doc.md")),
            other => panic!("expected gui, got {other:?}"),
        }
    }

    #[test]
    fn register_flags() {
        match parse(args(&["register", "--default"])) {
            Command::Register { set_default: true } => {}
            other => panic!("expected register --default, got {other:?}"),
        }
        match parse(args(&["register"])) {
            Command::Register { set_default: false } => {}
            other => panic!("expected register, got {other:?}"),
        }
    }

    #[test]
    fn help_and_version() {
        assert!(matches!(parse(args(&["--help"])), Command::Help));
        assert!(matches!(parse(args(&["-V"])), Command::Version));
    }
}
