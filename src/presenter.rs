//! Presenter layer: user intents and use-case logic.
//!
//! Owns the [`Model`] and exposes the actions the view can request.
//! Keeping this free of GUI types makes the logic unit-testable.

use std::path::{Path, PathBuf};

use crate::model::{Document, FileType, Model};

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
    // The shell document lives at about:blank; only the document itself
    // and same-document fragment clicks stay ("about:blank/rel.md" is a
    // relative link resolved against it — a different, blank document).
    if lower == "about:blank" || lower.starts_with("about:blank#") {
        return LinkAction::Stay;
    }
    LinkAction::Refuse
}

/// Owns the [`Model`] and exposes the actions the view can request.
/// Keeping this free of GUI types makes the logic unit-testable.
///
/// The model field is deliberately private: the view (and everything
/// else outside this module) reaches state only through these intents
/// and read accessors — the one-way street that keeps mutations in one
/// place and the view dumb. The compiler enforces it.
pub struct Presenter {
    model: Model,
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

    /// Resolve a link from the rendered pane into a file to open in a
    /// new window (VSCode-style): `file:` URLs and absolute paths
    /// percent-decoded as-is, anything else relative to the current
    /// document's directory. `None` for non-file links (web, mailto,
    /// `pound://`), anchors, and relative links with no document open.
    /// The file need not exist — the new window explains a miss itself.
    pub fn link_target(&self, href: &str) -> Option<PathBuf> {
        let href = href.trim();
        if href.is_empty() || href.starts_with('#') {
            return None;
        }
        match link_action(href, false) {
            LinkAction::Refuse => {} // a file target — resolve it below
            _ => return None,        // web, internal, anchor: no window to open
        }
        // `file:///C:/x%20y.md` -> `C:/x y.md` (scheme check
        // case-insensitive; the slice is safe — ASCII prefix).
        let raw = if href.to_ascii_lowercase().starts_with("file://") {
            &href[7..]
        } else {
            href
        };
        let decoded = crate::markdown::percent_decode(raw).unwrap_or_else(|| raw.to_owned());
        // file:///C:/… leaves a leading slash before the drive letter.
        let trimmed = decoded
            .strip_prefix('/')
            .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
            .unwrap_or(&decoded);
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Some(path.to_path_buf());
        }
        // Relative links resolve against the document's directory; with
        // no document open there is no base to resolve against.
        self.model
            .document
            .as_ref()
            .and_then(|doc| doc.path.parent())
            .map(|dir| dir.join(path))
    }

    pub fn dismiss_error(&mut self) {
        self.model.dismiss_error();
    }

    // ---- background rendering (see the view's render worker) ----

    /// Everything a background render of the current buffer needs: the
    /// file type (which renderer) and the document's directory (the base
    /// for image references). `None` with no document.
    pub fn render_spec(&self) -> Option<(FileType, PathBuf)> {
        self.model.document.as_ref().map(|doc| {
            (
                doc.file_type,
                doc.path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            )
        })
    }

    /// Take a background load's result (open, drop, initial file) — the
    /// async half of [`Presenter::open_path`].
    pub fn adopt(&mut self, result: Result<Document, String>) {
        self.model.adopt(result);
    }

    /// Take a background reload's result (see `Model::adopt_reload`).
    pub fn adopt_reload(&mut self, result: Result<Document, String>) -> bool {
        self.model.adopt_reload(result)
    }

    /// The cheap disk-change poll (see `Model::poll_disk_changed`): no
    /// file read — the view loads the file in the background when this
    /// turns `true`.
    pub fn poll_disk_changed(&mut self) -> bool {
        self.model.poll_disk_changed()
    }

    /// Store a new editor buffer without re-rendering (the render runs
    /// on the worker; its result arrives via [`Presenter::apply_render`]).
    /// Returns whether the buffer changed.
    pub fn edit_buffer(&mut self, text: &str) -> bool {
        self.model.edit_buffer(text)
    }

    /// Deliver a background render; returns whether the html (and so the
    /// view's re-push) changed.
    pub fn apply_render(&mut self, html: String) -> bool {
        self.model.apply_render(html)
    }

    /// Apply an edit from the source pane (drives the live preview).
    pub fn edit_source(&mut self, text: &str) {
        self.model.edit(text);
    }

    /// Save unsaved edits back to the file.
    pub fn save(&mut self) {
        self.model.save();
    }

    /// Export the open markdown document as a standalone HTML file next
    /// to its source (`notes.md` → `notes.html`; see `Model::export_html`).
    /// Returns the file written — the caller shows the success toast for
    /// it; `None` means no export happened (a failure banner, if any,
    /// travels through the usual error push).
    pub fn export_html(&mut self) -> Option<PathBuf> {
        self.model.export_html()
    }

    /// Called on a timer so external edits show up without a restart.
    pub fn reload_if_changed(&mut self) -> bool {
        self.model.reload_if_changed()
    }

    /// Handle files dragged onto the window: prefer markdown, then CSV,
    /// else the last thing dropped. Loads synchronously (tests, headless
    /// callers); the GUI uses [`Presenter::pick_dropped`] and loads the
    /// pick in the background.
    pub fn open_dropped(&mut self, paths: &[PathBuf]) {
        if let Some(path) = self.pick_dropped(paths) {
            self.open_path(&path);
        }
    }

    /// Choose which dropped file to open (the choice half of
    /// [`Presenter::open_dropped`], split so the view can load it off
    /// the UI thread): prefer markdown, then CSV, else the last thing
    /// dropped.
    pub fn pick_dropped(&self, paths: &[PathBuf]) -> Option<PathBuf> {
        let prefer = |kind: FileType| paths.iter().rev().find(|p| FileType::from_path(p) == kind);
        prefer(FileType::Markdown)
            .or_else(|| prefer(FileType::Csv))
            .or_else(|| paths.last())
            .cloned()
    }

    /// Sort the CSV table by a column (a header click in the shell); the
    /// same column toggles the direction. Returns whether the table
    /// changed (the caller re-pushes only then).
    pub fn sort_csv(&mut self, column: usize) -> bool {
        self.model.sort_csv(column)
    }

    /// Open a web/mailto link — or the export toast's file — in the
    /// system browser, recording a banner error when the OS refuses it.
    /// Returns whether it opened (the caller re-pushes the banner only
    /// on failure). Called from the Windows view (and its ipc handler)
    /// — hence the relaxed lint off-Windows.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn open_externally(&mut self, url: &str) -> bool {
        match open::that(url) {
            Ok(()) => true,
            Err(e) => {
                self.fail(format!("could not open {url}: {e}"));
                false
            }
        }
    }

    /// Record that a link was refused — following it would strand the
    /// reader on a blank page — so the banner explains the no-op.
    pub fn refuse_link(&mut self, url: &str) {
        self.fail(format!("link cannot be opened in the reader: {url}"));
    }

    /// Record a user-facing failure the view cannot handle itself
    /// (updater spawns, new-window spawns).
    pub fn fail(&mut self, message: String) {
        self.model.error = Some(message);
    }

    // ---- read accessors: everything the render step pulls ----

    /// Whether unsaved edits are in the buffer (drives the close prompt,
    /// the `*` title prefix and the status bar).
    pub fn is_dirty(&self) -> bool {
        self.model.dirty
    }

    /// The document revision, bumped on every load and edit; the view
    /// watches it to know when to re-push content.
    pub fn revision(&self) -> u64 {
        self.model.revision
    }

    /// The open document's path (the reopen-after-update target), or
    /// `None` with no document.
    pub fn document_path(&self) -> Option<PathBuf> {
        self.model.document.as_ref().map(|doc| doc.path.clone())
    }

    /// The last user-facing error, or `""` when there is none.
    pub fn error_text(&self) -> &str {
        self.model.error.as_deref().unwrap_or("")
    }

    /// The rendered preview HTML; empty with no document or for
    /// plain-text files (the source pane is the view).
    pub fn html(&self) -> &str {
        self.model
            .document
            .as_ref()
            .map_or("", |doc| doc.html.as_str())
    }

    /// The editor buffer; empty with no document.
    pub fn source(&self) -> &str {
        self.model
            .document
            .as_ref()
            .map_or("", |doc| doc.source.as_str())
    }

    /// The document's path as shown to users (status bar); empty with
    /// no document.
    pub fn display_path(&self) -> String {
        self.model
            .document
            .as_ref()
            .map_or_else(String::new, |doc| doc.path.display().to_string())
    }

    /// The pane layout for the current document, as the shell knows it:
    /// `"markdown"` (preview + source), `"plain"` (source only), `"csv"`
    /// (rendered table only). `"markdown"` doubles as the no-document
    /// shape (the welcome screen).
    pub fn view_mode(&self) -> &'static str {
        match self.model.document.as_ref().map(|doc| doc.file_type) {
            Some(FileType::Csv) => "csv",
            Some(FileType::Code(_)) => "code",
            Some(FileType::Plain) => "plain",
            Some(FileType::Markdown) | None => "markdown",
        }
    }

    /// The window title: the document's name, `*`-prefixed while edits
    /// are unsaved (the editor convention), or just `Pound`.
    pub fn title(&self) -> String {
        match &self.model.document {
            Some(doc) if self.model.dirty => format!("*{} — Pound", doc.name()),
            Some(doc) => format!("{} — Pound", doc.name()),
            None => "Pound".to_owned(),
        }
    }

    /// Status-bar text: file type, character and line counts, the edit
    /// state, and the app version — e.g. `Markdown · 1,234 characters · 56
    /// lines · edited · v0.4.0`.
    pub fn status_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(doc) = &self.model.document {
            if let Some((rows, columns)) = doc.shape {
                // Tables: the shape is the friendly stat (character and
                // line counts are noise for data).
                parts.push(format!(
                    "CSV · {} rows · {} columns",
                    group_digits(rows),
                    group_digits(columns)
                ));
            } else {
                parts.push(format!(
                    "{} · {} characters · {} lines",
                    doc.type_label(),
                    group_digits(doc.source.chars().count()),
                    group_digits(doc.source.lines().count())
                ));
            }
        }
        if self.model.dirty {
            parts.push("edited".to_owned());
        }
        if self.model.stale {
            parts.push("changed on disk".to_owned());
        }
        // The last export's target rides along until the document
        // changes (exports are otherwise silent).
        if let Some(notice) = &self.model.notice {
            parts.push(notice.clone());
        }
        parts.push(format!("v{}", env!("CARGO_PKG_VERSION")));
        parts.join(" · ")
    }
}

/// Group digits in threes (1234567 -> 1,234,567).
fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
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
        assert_eq!(link_action("about:blank/gone.md", false), Refuse); // relative link, resolved
        assert_eq!(link_action("data:text/html,hi", false), Refuse);
        assert_eq!(link_action("poundimg://C:/x.png", false), Refuse);
        assert_eq!(link_action("ftp://host/f", false), Refuse);
        assert_eq!(link_action("relative/other.md", false), Refuse);
    }

    #[test]
    fn link_targets_resolve_like_the_document_directory() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("doc.md");
        std::fs::write(&doc, "# hi").unwrap();
        let presenter = Presenter::new(Some(&doc));

        // Relative links resolve against the document's directory; missing
        // files are fine — the new window explains a miss itself. Expect
        // the document's own (canonicalized) directory: temp dirs can
        // arrive in 8.3 short form (RUNNER~1) while Document::load
        // canonicalizes to the long form — string paths would differ.
        let doc_dir = presenter
            .model
            .document
            .as_ref()
            .unwrap()
            .path
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(
            presenter.link_target("notes/gone.md"),
            Some(doc_dir.join("notes/gone.md"))
        );
        // file: URLs decode and drop the leading slash before a drive.
        #[cfg(windows)]
        assert_eq!(
            presenter.link_target("file:///C:/my%20docs/x.md"),
            Some(PathBuf::from("C:/my docs/x.md"))
        );
        #[cfg(not(windows))]
        assert_eq!(
            presenter.link_target("file:///tmp/my%20docs/x.md"),
            Some(PathBuf::from("/tmp/my docs/x.md"))
        );
        // Absolute paths stay as-is (host-appropriate form).
        #[cfg(windows)]
        assert_eq!(
            presenter.link_target("C:/abs.md"),
            Some(PathBuf::from("C:/abs.md"))
        );
        #[cfg(not(windows))]
        assert_eq!(
            presenter.link_target("/abs.md"),
            Some(PathBuf::from("/abs.md"))
        );
        // Non-file links and anchors never open a window.
        assert_eq!(presenter.link_target("https://example.com"), None);
        assert_eq!(presenter.link_target("mailto:a@b.c"), None);
        assert_eq!(presenter.link_target("pound://dismiss-error"), None);
        assert_eq!(presenter.link_target("#anchor"), None);
        assert_eq!(presenter.link_target(""), None);

        // With no document open there is nothing to resolve against
        // (absolute paths still open — they carry their own base).
        let empty = Presenter::new(None);
        assert_eq!(empty.link_target("gone.md"), None);
        #[cfg(windows)]
        assert_eq!(
            empty.link_target("C:/abs.md"),
            Some(PathBuf::from("C:/abs.md"))
        );
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

        // The pick (the async half) chooses without opening.
        let chooser = Presenter::new(None);
        assert_eq!(
            chooser.pick_dropped(&[PathBuf::from("/tmp/z.csv"), PathBuf::from("/tmp/b.md")]),
            Some(PathBuf::from("/tmp/b.md"))
        );
        assert_eq!(chooser.pick_dropped(&[]), None);
    }

    #[test]
    fn background_flow_mirrors_the_synchronous_one() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("async.md");
        std::fs::write(&path, "# hi").unwrap();

        // open_path (sync) and adopt(load) (async) land in the same state.
        let sync = Presenter::new(Some(&path));
        let mut async_p = Presenter::new(None);
        async_p.adopt(Document::load(&path));
        assert_eq!(async_p.html(), sync.html());
        assert_eq!(async_p.revision(), sync.revision());

        // The render spec describes the open document.
        let (file_type, base) = async_p.render_spec().unwrap();
        assert_eq!(file_type, FileType::Markdown);
        assert_eq!(
            base,
            sync.document_path()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf()
        );
        assert!(Presenter::new(None).render_spec().is_none());

        // Buffer-first/apply-later keeps dirty and revision honest.
        assert!(async_p.edit_buffer("# hi there"));
        assert!(async_p.is_dirty());
        assert_eq!(async_p.revision(), sync.revision());
        let html = crate::model::render_source("# hi there", FileType::Markdown, &base);
        assert!(async_p.apply_render(html.clone()));
        assert!(async_p.html().contains("there"));
        assert!(!async_p.apply_render(html), "identical html: no re-push");

        // The wrappers save and adopt through the presenter too.
        async_p.save();
        assert!(!async_p.is_dirty(), "save ack clears the dirty marker");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# hi there");
        assert!(!async_p.poll_disk_changed());
        assert!(!async_p.adopt_reload(Document::load(&path)));
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

    #[test]
    fn errors_route_through_the_presenter() {
        let mut presenter = Presenter::new(None);
        assert_eq!(presenter.error_text(), "");
        presenter.fail("boom".to_owned());
        assert_eq!(presenter.error_text(), "boom");
        presenter.refuse_link("data:text/html,hi");
        assert_eq!(
            presenter.error_text(),
            "link cannot be opened in the reader: data:text/html,hi"
        );
        presenter.dismiss_error();
        assert_eq!(presenter.error_text(), "");
    }

    #[test]
    fn titles_track_the_document_and_edit_state() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("titles.md");
        std::fs::write(&path, "# hi").unwrap();

        let mut presenter = Presenter::new(None);
        assert_eq!(presenter.title(), "Pound");
        presenter.open_path(&path);
        assert_eq!(presenter.title(), "titles — Pound");
        presenter.edit_source("# hi there");
        assert_eq!(presenter.title(), "*titles — Pound");
    }

    #[test]
    fn status_text_reports_counts_and_states() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("status.md");
        std::fs::write(&path, "# hi\nbody").unwrap();
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));

        // No document: just the version.
        let presenter = Presenter::new(None);
        assert_eq!(presenter.status_text(), version);

        let mut presenter = Presenter::new(Some(&path));
        // "# hi\nbody": 9 characters, 2 lines.
        assert_eq!(
            presenter.status_text(),
            format!("Markdown · 9 characters · 2 lines · {version}")
        );

        presenter.edit_source("# hi\nbody — more");
        assert_eq!(
            presenter.status_text(),
            format!("Markdown · 16 characters · 2 lines · edited · {version}")
        );

        presenter.model.stale = true;
        assert_eq!(
            presenter.status_text(),
            format!("Markdown · 16 characters · 2 lines · edited · changed on disk · {version}")
        );
    }

    #[test]
    fn view_accessors_expose_model_state() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view.md");
        std::fs::write(&path, "# hello").unwrap();

        // No document: empty view state, nothing dirty.
        let mut presenter = Presenter::new(None);
        assert_eq!(presenter.html(), "");
        assert_eq!(presenter.source(), "");
        assert_eq!(presenter.display_path(), "");
        assert_eq!(presenter.view_mode(), "markdown");
        assert!(!presenter.is_dirty());
        assert_eq!(presenter.revision(), 0);
        assert_eq!(presenter.document_path(), None);

        presenter.open_path(&path);
        assert!(presenter.html().contains("<h1>"));
        assert_eq!(presenter.source(), "# hello");
        assert_eq!(presenter.revision(), 1);
        assert!(!presenter.is_dirty());
        assert_eq!(presenter.view_mode(), "markdown");
        assert_eq!(
            presenter.document_path().unwrap().file_name(),
            Some(std::ffi::OsStr::new("view.md"))
        );
        assert!(presenter.display_path().ends_with("view.md"));

        presenter.edit_source("# hello world");
        assert!(presenter.is_dirty());
        assert_eq!(presenter.revision(), 2);

        // Plain-text files render nothing: source pane alone.
        let txt = dir.join("view.txt");
        std::fs::write(&txt, "just text").unwrap();
        presenter.open_path(&txt);
        assert_eq!(presenter.html(), "");
        assert_eq!(presenter.source(), "just text");
        assert_eq!(presenter.view_mode(), "plain");

        // Code files render highlighted, in the editor's overlay.
        let toml = dir.join("view.toml");
        std::fs::write(&toml, "key = 1\n").unwrap();
        presenter.open_path(&toml);
        assert_eq!(presenter.view_mode(), "code");
        assert!(presenter.html().contains("language-toml"));
        assert_eq!(presenter.source(), "key = 1\n");
    }

    #[test]
    fn csv_documents_are_read_only_tables() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("table.csv");
        std::fs::write(&path, "name,age\nada,36\nbob,25\n").unwrap();
        let mut presenter = Presenter::new(Some(&path));

        // Table-only layout, read-only by construction.
        assert_eq!(presenter.view_mode(), "csv");
        assert!(presenter.html().contains("<table class=\"csv\">"));
        assert_eq!(presenter.source(), "name,age\nada,36\nbob,25\n");
        assert!(!presenter.is_dirty());

        // The status bar counts rows and columns, not characters.
        assert_eq!(
            presenter.status_text(),
            format!("CSV · 2 rows · 2 columns · v{}", env!("CARGO_PKG_VERSION"))
        );

        // Sorting flows through the presenter and re-renders (toggling
        // on a second click of the same header).
        assert!(presenter.sort_csv(0));
        assert!(presenter.html().contains("sort-asc"));
        assert!(presenter.sort_csv(0));
        assert!(presenter.html().contains("sort-desc"));
        assert!(!presenter.is_dirty());
    }

    #[test]
    fn export_html_reports_the_target_in_the_status_bar() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("exported.md");
        std::fs::write(&path, "# hi").unwrap();
        let mut presenter = Presenter::new(Some(&path));

        // No document, nothing to export; a markdown document reports
        // the written file until the next edit.
        assert!(Presenter::new(None).export_html().is_none());
        let target = presenter.export_html().unwrap();
        assert_eq!(
            target.file_name(),
            Some(std::ffi::OsStr::new("exported.html"))
        );
        assert!(presenter
            .status_text()
            .contains("exported to exported.html"));
        presenter.edit_source("# bye");
        assert!(!presenter.status_text().contains("exported to"));
        assert!(std::fs::read_to_string(path.with_extension("html"))
            .unwrap()
            .contains("<h1>hi</h1>"));
    }

    #[test]
    fn digits_group_in_threes() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
    }
}
