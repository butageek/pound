//! Presenter layer: user intents and use-case logic.
//!
//! Owns the [`Model`] and exposes the actions the view can request.
//! Keeping this free of egui types makes the logic unit-testable.

use std::path::{Path, PathBuf};

use crate::model::Model;

/// URL schemes we are willing to hand to the OS when a link is clicked.
const LINK_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

pub struct Presenter {
    pub model: Model,
}

impl Presenter {
    pub fn new(initial_file: Option<&Path>) -> Presenter {
        let mut presenter = Presenter {
            model: Model::default(),
        };
        if let Some(path) = initial_file {
            presenter.open_path(path);
        }
        presenter
    }

    pub fn open_path(&mut self, path: &Path) {
        self.model.open(path);
    }

    pub fn toggle_source(&mut self) {
        self.model.toggle_source();
    }

    pub fn set_show_source(&mut self, show: bool) {
        self.model.set_show_source(show);
    }

    pub fn dismiss_error(&mut self) {
        self.model.dismiss_error();
    }

    /// Reopen the current document from disk.
    pub fn reload(&mut self) {
        self.model.reload();
    }

    /// Called on a timer so external edits show up without a restart.
    pub fn reload_if_changed(&mut self) {
        self.model.reload_if_changed();
    }

    /// Handle files dragged onto the window: prefer markdown files.
    pub fn open_dropped(&mut self, paths: &[PathBuf]) {
        let Some(path) = paths
            .iter()
            .rev()
            .find(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()),
                    Some(ext) if ext == "md" || ext == "markdown"
                )
            })
            .or_else(|| paths.last())
        else {
            return;
        };
        self.open_path(path);
    }

    /// Open a link from the rendered document in the system browser.
    pub fn open_link(&mut self, url: &str) {
        let scheme_ok = url
            .split_once(':')
            .map(|(scheme, _)| LINK_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()))
            .unwrap_or(false);
        if !scheme_ok {
            self.model.error = Some(format!("refusing to open link with unknown scheme: {url}"));
            return;
        }
        if let Err(e) = open::that(url) {
            self.model.error = Some(format!("could not open {url}: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Model;

    #[test]
    fn initial_file_is_loaded() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("init.md");
        std::fs::write(&path, "# hi").unwrap();
        let presenter = Presenter::new(Some(&path));
        assert!(presenter.model.document.is_some());
    }

    #[test]
    fn dropped_files_prefer_markdown() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("notes.md");
        std::fs::write(&md, "# notes").unwrap();

        let mut presenter = Presenter {
            model: Model::default(),
        };
        presenter.open_dropped(&[PathBuf::from("/tmp/a.txt"), md.clone()]);
        assert_eq!(
            presenter.model.document.as_ref().unwrap().path,
            md.canonicalize().unwrap()
        );
    }

    #[test]
    fn unknown_link_scheme_is_refused() {
        let mut presenter = Presenter {
            model: Model::default(),
        };
        presenter.open_link("file:///etc/passwd");
        assert!(presenter.model.error.is_some());
        presenter.dismiss_error();
        assert!(presenter.model.error.is_none());
    }
}
