//! Presenter layer: user intents and use-case logic.
//!
//! Owns the [`Model`] and exposes the actions the view can request.
//! Keeping this free of GUI types makes the logic unit-testable.

use std::path::{Path, PathBuf};

use crate::model::Model;

/// What a navigation request in the rendered pane should do.
///
/// The reader has no back button by design, so the one rule that
/// matters: once the shell document is up, the webview must never
/// navigate anywhere else — a `file:` link, a `data:` URL or a relative
/// path would strand the app on a blank page.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkAction {
    /// Navigate within the shell document: the initial load and in-page
    /// anchors (`about:blank#section`).
    Stay,
    /// Open outside the reader, in the system browser.
    OpenExternally,
    /// A `pound://` control URL handled by the view itself.
    Internal,
    /// Refuse and tell the user why.
    Refuse,
}

/// Classify a navigation URL. `initial_load` must be true only for the
/// handler's first call — whatever URL the platform reports for the
/// shell document itself is allowed, sight unseen.
pub fn link_action(url: &str, initial_load: bool) -> LinkAction {
    if initial_load || url.is_empty() {
        return LinkAction::Stay;
    }
    let lower = url.to_ascii_lowercase();
    // All scheme checks are case-insensitive; the shell emits its own
    // control URLs lowercase, but a file's raw HTML could do otherwise.
    if lower.starts_with("pound://") {
        return LinkAction::Internal;
    }
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        return LinkAction::OpenExternally;
    }
    // The shell document lives at about:blank; fragment clicks on
    // in-page anchors navigate within it.
    if lower.starts_with("about:") {
        return LinkAction::Stay;
    }
    LinkAction::Refuse
}

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
    fn links_are_classified_for_navigation() {
        use LinkAction::*;
        // The handler's first call is the shell document itself — allowed
        // whatever it looks like.
        assert_eq!(link_action("data:text/html,<html>…", true), Stay);
        // Web links open externally; controls are internal.
        assert_eq!(link_action("https://example.com", false), OpenExternally);
        assert_eq!(link_action("HTTP://Example.COM", false), OpenExternally);
        assert_eq!(link_action("mailto:a@b.c", false), OpenExternally);
        assert_eq!(link_action("pound://dismiss-error", false), Internal);
        // In-page anchors stay within the shell document.
        assert_eq!(link_action("about:blank#section", false), Stay);
        assert_eq!(link_action("", false), Stay);
        // Everything else would strand the reader on a blank page.
        assert_eq!(link_action("file:///C:/gone.md", false), Refuse);
        assert_eq!(link_action("data:text/html,hi", false), Refuse);
        assert_eq!(link_action("poundimg://C:/x.png", false), Refuse);
        assert_eq!(link_action("ftp://host/f", false), Refuse);
        assert_eq!(link_action("relative/other.md", false), Refuse);
    }

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
