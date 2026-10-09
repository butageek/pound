//! Presenter layer: user intents and use-case logic.
//!
//! Owns the [`Model`] and exposes the actions the view can request.
//! Keeping this free of GUI types makes the logic unit-testable.

use std::path::{Path, PathBuf};

use crate::model::Model;

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

    pub fn dismiss_error(&mut self) {
        self.model.dismiss_error();
    }

    /// Apply an edit from the source pane (drives the live preview).
    pub fn edit_source(&mut self, text: &str) {
        self.model.edit(text);
    }

    /// Save unsaved edits back to the file.
    pub fn save(&mut self) {
        self.model.save();
    }

    /// Called on a timer so external edits show up without a restart.
    pub fn reload_if_changed(&mut self) -> bool {
        self.model.reload_if_changed()
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
        // On Windows `canonicalize` returns `\\?\`-prefixed verbatim paths,
        // which `Document::load` strips — compare against the stripped form
        // (raw output matches on Linux, so this only fails on Windows CI).
        assert_eq!(
            presenter.model.document.as_ref().unwrap().path,
            crate::model::strip_verbatim_prefix(md.canonicalize().unwrap())
        );
    }

    #[test]
    fn watch_is_a_noop_without_a_document() {
        let mut presenter = Presenter {
            model: Model::default(),
        };
        assert!(!presenter.reload_if_changed());
        assert!(presenter.model.error.is_none());
    }

    #[test]
    fn errors_can_be_dismissed() {
        let mut presenter = Presenter {
            model: Model::default(),
        };
        presenter.model.error = Some("boom".to_owned());
        presenter.dismiss_error();
        assert!(presenter.model.error.is_none());
    }
}
