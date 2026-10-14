//! Application state (Model layer). No UI dependencies here.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::markdown;

/// A loaded markdown document: source text plus its rendered HTML.
pub struct Document {
    pub path: PathBuf,
    pub source: String,
    pub html: String,
    pub modified: Option<SystemTime>,
    /// The file used CRLF line endings. The editor buffer is LF (the DOM
    /// normalizes), so saving converts back to keep diffs noise-free.
    pub crlf: bool,
    /// How this file renders and lays out (one source of truth for the
    /// renderer, the pane layout and `type_label`).
    pub file_type: FileType,
    /// The file's shape for CSV documents — `(data rows, columns)`, the
    /// whole file's, never the rendered cap; `None` otherwise.
    pub shape: Option<(usize, usize)>,
    /// The parsed table (CSV text): the renderer's input, kept so
    /// re-sorting never re-reads the file. `None` for non-table
    /// documents.
    pub grid: Option<Vec<Vec<String>>>,
    /// The CSV table's current sort (render state; drives re-rendering
    /// on header clicks). `None` until the user sorts.
    pub csv_sort: Option<crate::csv::Sort>,
}

impl Document {
    pub fn load(path: &Path) -> Result<Document, String> {
        let absolute = strip_verbatim_prefix(
            path.canonicalize()
                .map_err(|e| format!("cannot open {}: {e}", path.display()))?,
        );
        let bytes =
            fs::read(&absolute).map_err(|e| format!("cannot read {}: {e}", absolute.display()))?;
        let file_type = FileType::from_path(&absolute);
        let (source, grid) = match file_type {
            // CSV parses into a grid at load; re-sorting re-renders from
            // the grid without touching the file again.
            FileType::Csv => {
                let source = decode_text(&bytes, &absolute)?;
                let grid = crate::csv::parse_grid(&source);
                (source, Some(grid))
            }
            FileType::Markdown | FileType::Plain => (decode_text(&bytes, &absolute)?, None),
        };
        let modified = fs::metadata(&absolute).and_then(|m| m.modified()).ok();
        let crlf = source.contains("\r\n");
        let (html, shape) = match file_type {
            // Table documents always carry the grid parsed above;
            // markdown renders text and plain files render nothing.
            FileType::Csv => crate::csv::grid_to_html(grid.as_deref().unwrap_or(&[]), None),
            FileType::Markdown => (
                markdown::to_html(&source, absolute.parent().unwrap_or(Path::new("."))),
                None,
            ),
            FileType::Plain => (String::new(), None),
        };
        Ok(Document {
            html,
            path: absolute,
            source,
            modified,
            crlf,
            file_type,
            shape,
            grid,
            csv_sort: None,
        })
    }

    /// File name without extension, for the window title.
    pub fn name(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string())
    }

    /// Human-facing file type for the status bar, e.g. `Markdown`, `CSV`
    /// or `TXT file`.
    pub fn type_label(&self) -> String {
        match self.file_type {
            FileType::Markdown => "Markdown".to_owned(),
            FileType::Csv => "CSV".to_owned(),
            FileType::Plain => {
                let ext = self
                    .path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match ext.as_str() {
                    "txt" => "Plain text".to_owned(),
                    "" => "File".to_owned(),
                    other => format!("{} file", other.to_ascii_uppercase()),
                }
            }
        }
    }
}

/// Decode file bytes as UTF-8 text. Binary files (PDFs, .docx, images…)
/// get a message that says what to do instead of Rust's "stream did not
/// contain valid UTF-8".
fn decode_text(bytes: &[u8], path: &Path) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        format!(
            "cannot open {}: not a text file (Pound reads markdown and CSV)",
            path.display()
        )
    })
}

/// The kinds of files Pound treats differently. One source of truth for
/// the renderer, the pane layout and `type_label` (this enum replaces
/// the old `is_markdown_path` booleans — CSV is why it exists).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    /// Rendered preview plus an editable source pane, side by side.
    Markdown,
    /// Delimited text (`.csv`/`.tsv`) as a sanitized, sortable table —
    /// the rendered pane is the whole view (data, not prose: no source
    /// pane, nothing to edit).
    Csv,
    /// An editable source pane alone (a text file is its own source).
    Plain,
}

impl FileType {
    pub fn from_path(path: &Path) -> FileType {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        match ext.as_deref() {
            Some("md" | "markdown") => FileType::Markdown,
            Some("csv" | "tsv") => FileType::Csv,
            _ => FileType::Plain,
        }
    }
}

/// `Path::canonicalize` returns Windows verbatim paths (`\\?\C:\…`,
/// `\\?\UNC\server\share`), which read as gibberish to users. Unwrap the
/// prefix so every display site shows ordinary paths (`C:\…`,
/// `\\server\share`); the plain form is accepted by every filesystem API
/// just as well. No-op on other platforms.
pub(crate) fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_owned());
    }
    path
}

/// All mutable application state.
#[derive(Default)]
pub struct Model {
    pub document: Option<Document>,
    /// Last user-facing error, shown as a banner.
    pub error: Option<String>,
    /// Last completed export's target (shown in the status bar until the
    /// document changes — exports are otherwise silent).
    pub notice: Option<String>,
    /// Bumped every time the document is (re)loaded or edited; lets the
    /// view invalidate caches such as loaded image textures.
    pub revision: u64,
    /// Unsaved edits are in memory (the source pane is an editor).
    pub dirty: bool,
    /// The file changed on disk while unsaved edits exist; auto-reload is
    /// paused until they are saved (unsaved edits always win).
    pub stale: bool,
}

impl Model {
    pub fn open(&mut self, path: &Path) {
        match Document::load(path) {
            Ok(doc) => {
                self.document = Some(doc);
                self.error = None;
                self.notice = None;
                self.dirty = false;
                self.stale = false;
                self.revision += 1;
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn reload(&mut self) {
        if let Some(doc) = &self.document {
            let path = doc.path.clone();
            self.open(&path);
        }
    }

    pub fn dismiss_error(&mut self) {
        self.error = None;
    }

    /// Apply an edit from the source pane: replace the buffer, re-render
    /// the preview and remember that it is unsaved. Identical text is a
    /// no-op (no spurious re-render or revision bump).
    pub fn edit(&mut self, text: &str) {
        let Some(doc) = self.document.as_mut() else {
            return;
        };
        if doc.source == text {
            return;
        }
        doc.source = text.to_owned();
        // Plain files render nothing (the source pane is the view), so
        // this is safe for every editable kind. CSV documents are
        // read-only: the shell posts no edits for them — there is no
        // source pane to type in — so their grid never goes stale.
        doc.html = match doc.file_type {
            FileType::Markdown => {
                markdown::to_html(&doc.source, doc.path.parent().unwrap_or(Path::new(".")))
            }
            _ => String::new(),
        };
        self.revision += 1;
        self.dirty = true;
        self.notice = None;
    }

    /// Export the open markdown document as a standalone HTML file next
    /// to its source (`notes.md` → `notes.html`, overwriting an earlier
    /// export). Renders the live buffer, so unsaved edits ship too.
    /// Non-markdown documents and missing documents are no-ops. Returns
    /// whether a notice or error was recorded (the caller refreshes the
    /// status line then).
    pub fn export_html(&mut self) -> bool {
        let Some(doc) = &self.document else {
            return false;
        };
        if !matches!(doc.file_type, FileType::Markdown) {
            return false;
        }
        let target = doc.path.with_extension("html");
        let page = crate::export::standalone_html(
            &doc.source,
            doc.path.parent().unwrap_or(Path::new(".")),
            &doc.name(),
        );
        match fs::write(&target, page) {
            Ok(()) => {
                // The target always has a file name (it derives from a
                // loaded file's path); a miss would just skip the notice.
                self.notice = target
                    .file_name()
                    .map(|name| format!("exported to {}", name.to_string_lossy()));
                true
            }
            Err(e) => {
                self.error = Some(format!("cannot export {}: {e}", target.display()));
                true
            }
        }
    }

    /// Write unsaved edits back to disk, preserving the file's original
    /// line-ending style. Last writer wins if the file changed on disk.
    pub fn save(&mut self) {
        let Some(doc) = &self.document else { return };
        if !self.dirty {
            return;
        }
        let text = if doc.crlf {
            doc.source.replace('\n', "\r\n")
        } else {
            doc.source.clone()
        };
        let written = fs::write(&doc.path, text)
            .and_then(|_| fs::metadata(&doc.path))
            .and_then(|m| m.modified());
        match written {
            Ok(modified) => {
                self.dirty = false;
                self.stale = false;
                self.notice = None;
                if let Some(doc) = self.document.as_mut() {
                    doc.modified = Some(modified);
                }
            }
            Err(e) => self.error = Some(format!("cannot save {}: {e}", doc.path.display())),
        }
    }

    /// Reload the document if its modification time changed on disk and
    /// there is nothing unsaved to protect. Returns `true` when the
    /// document was reloaded.
    pub fn reload_if_changed(&mut self) -> bool {
        let Some(doc) = &self.document else {
            return false;
        };
        let Ok(meta) = fs::metadata(&doc.path) else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        if doc.modified == Some(modified) {
            return false;
        }
        // Unsaved edits always win over the disk: pause auto-reload and
        // surface the conflict in the status bar instead of clobbering.
        if self.dirty {
            self.stale = true;
            return false;
        }
        // An mtime touch with identical content (e.g. a save from another
        // Pound window) must not reset the view for no visible reason.
        if fs::read(&doc.path).is_ok_and(|disk| String::from_utf8_lossy(&disk) == doc.source) {
            if let Some(doc) = self.document.as_mut() {
                doc.modified = Some(modified);
            }
            return false;
        }
        self.reload();
        true
    }

    /// Re-render the CSV table under a new sort (a header click): the
    /// same column toggles the direction. Works from the stored grid, so
    /// no file re-read. Pure view state — never dirty, never saved.
    /// Returns whether anything changed.
    pub fn sort_csv(&mut self, column: usize) -> bool {
        let Some(doc) = self.document.as_mut() else {
            return false;
        };
        if !matches!(doc.file_type, FileType::Csv) {
            return false;
        }
        // Same column again flips the direction; a new column starts
        // ascending.
        let ascending = match doc.csv_sort {
            Some(sort) if sort.column == column => !sort.ascending,
            _ => true,
        };
        doc.csv_sort = Some(crate::csv::Sort { column, ascending });
        let (html, _) = crate::csv::grid_to_html(doc.grid.as_deref().unwrap_or(&[]), doc.csv_sort);
        doc.html = html;
        self.revision += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    fn temp_md(name: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn open_loads_and_renders_html() {
        let path = temp_md("doc.md", "# Hello\n\nworld **bold**");
        let mut model = Model::default();
        model.open(&path);
        let doc = model.document.as_ref().expect("document loaded");
        assert_eq!(doc.name(), "doc");
        assert_eq!(doc.source, "# Hello\n\nworld **bold**");
        assert!(doc.html.contains("<h1>Hello</h1>"));
        assert!(doc.html.contains("<strong>bold</strong>"));
        assert_eq!(model.error, None);
        assert_eq!(model.revision, 1);
    }

    #[test]
    fn open_missing_file_sets_error() {
        let mut model = Model::default();
        model.open(Path::new("/definitely/not/here.md"));
        assert!(model.document.is_none());
        assert!(model.error.is_some());
    }

    #[test]
    fn non_markdown_files_open_in_the_source_pane() {
        // Plain files render nothing (html stays empty): the source pane
        // is the whole view, so markdown-ish characters stay inert by
        // construction and edits never re-render.
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let txt = dir.join("log.txt");
        std::fs::write(&txt, "# not a heading\n*blob* <tag>\n").unwrap();
        let mut model = Model::default();
        model.open(&txt);
        let doc = model.document.as_ref().unwrap();
        assert_eq!(doc.file_type, FileType::Plain);
        assert_eq!(doc.html, "");
        assert_eq!(doc.source, "# not a heading\n*blob* <tag>\n");

        model.edit("# still plain <x>\n");
        let doc = model.document.as_ref().unwrap();
        assert_eq!(doc.html, "", "edits never render: {}", doc.html);
        assert_eq!(doc.source, "# still plain <x>\n");

        // ...while actual markdown keeps rendering as markdown (a distinct
        // file name: tests in this module share the temp dir and run in
        // parallel).
        let md = dir.join("real.md");
        std::fs::write(&md, "# real heading\n").unwrap();
        model.open(&md);
        let doc = model.document.as_ref().unwrap();
        assert_eq!(doc.file_type, FileType::Markdown);
        assert!(doc.html.contains("<h1>"));
    }

    #[test]
    fn file_types_come_from_extensions() {
        let f = |name: &str| FileType::from_path(&PathBuf::from(name));
        assert_eq!(f("notes.md"), FileType::Markdown);
        assert_eq!(f("NOTES.MARKDOWN"), FileType::Markdown); // case-insensitive
        assert_eq!(f("data.csv"), FileType::Csv);
        assert_eq!(f("data.TSV"), FileType::Csv);
        assert_eq!(f("log.txt"), FileType::Plain);
        assert_eq!(f("Makefile"), FileType::Plain); // no extension
        assert_eq!(f("archive.tar.gz"), FileType::Plain);
    }

    #[test]
    fn csv_documents_render_a_table() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.csv");
        fs::write(&path, "name,age\nada,36\nbob,25\n").unwrap();
        let mut model = Model::default();
        model.open(&path);
        let doc = model.document.as_ref().unwrap();
        assert_eq!(doc.file_type, FileType::Csv);
        assert_eq!(doc.type_label(), "CSV");
        assert_eq!(doc.shape, Some((2, 2)));
        assert!(doc.html.contains("<table class=\"csv\">"));
        assert!(doc.html.contains("<th>name</th>"));
        // Read-only by construction: no sort or edit ever marks it dirty.
        assert!(!model.dirty);
    }

    #[test]
    fn sort_csv_re_renders_and_toggles() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sortable.csv");
        fs::write(&path, "n\n2\n1\n").unwrap();
        let mut model = Model::default();
        model.open(&path);
        let revision = model.revision;

        assert!(model.sort_csv(0));
        assert_eq!(model.revision, revision + 1);
        assert!(model.document.as_ref().unwrap().html.contains("sort-asc"));
        assert!(!model.dirty, "sorting is view state, never an edit");

        // Same column again: the direction flips.
        assert!(model.sort_csv(0));
        assert!(model.document.as_ref().unwrap().html.contains("sort-desc"));

        // Non-CSV documents ignore it entirely.
        let md = dir.join("plain-sort.md");
        fs::write(&md, "# hi").unwrap();
        model.open(&md);
        assert!(!model.sort_csv(0));
    }

    #[test]
    fn binary_files_get_an_actionable_error() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("brochure.pdf");
        fs::write(&path, [0x25, 0x50, 0x44, 0x46, 0x00, 0x01, 0x02, 0xff]).unwrap();
        let mut model = Model::default();
        model.open(&path);
        let error = model.error.as_deref().unwrap_or("");
        assert!(error.contains("not a text file"), "{error}");
        assert!(model.document.is_none());
    }

    #[test]
    fn verbatim_prefix_is_stripped_after_canonicalize() {
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\C:\Users\me\backup plan\notes.md")),
            PathBuf::from(r"C:\Users\me\backup plan\notes.md")
        );
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share\notes.md")),
            PathBuf::from(r"\\server\share\notes.md")
        );
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from("/home/me/notes.md")),
            PathBuf::from("/home/me/notes.md")
        );
    }

    #[test]
    fn type_label_names_common_types() {
        fn doc_with(ext: &str) -> Document {
            let path = PathBuf::from(format!("C:\\notes.{ext}"));
            Document {
                path: path.clone(),
                source: String::new(),
                html: String::new(),
                modified: None,
                crlf: false,
                file_type: FileType::from_path(&path),
                shape: None,
                grid: None,
                csv_sort: None,
            }
        }
        assert_eq!(doc_with("md").type_label(), "Markdown");
        assert_eq!(doc_with("markdown").type_label(), "Markdown");
        assert_eq!(doc_with("txt").type_label(), "Plain text");
        assert_eq!(doc_with("json").type_label(), "JSON file");
        assert_eq!(doc_with("csv").type_label(), "CSV");
        assert_eq!(doc_with("").type_label(), "File");
    }

    /// Force a new mtime on `path` (some filesystems share timestamps,
    /// so rewriting the file alone may not change it).
    fn bump_mtime(path: &Path) {
        let later = SystemTime::now() + Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(later)
            .unwrap();
    }

    #[test]
    fn edits_rerender_and_mark_dirty() {
        let path = temp_md("edit.md", "# v1");
        let mut model = Model::default();
        model.open(&path);
        let before = model.revision;

        model.edit(
            "# v2

new **body**",
        );
        let doc = model.document.as_ref().unwrap();
        assert_eq!(
            doc.source,
            "# v2

new **body**"
        );
        assert!(doc.html.contains("<h1>v2</h1>"));
        assert!(doc.html.contains("<strong>body</strong>"));
        assert!(model.dirty);
        assert_eq!(model.revision, before + 1);

        // Identical text is a no-op (no spurious re-render/revision bump).
        model.edit(
            "# v2

new **body**",
        );
        assert_eq!(model.revision, before + 1);
    }

    #[test]
    fn save_writes_edits_and_clears_dirty() {
        let path = temp_md("save.md", "# v1");
        let mut model = Model::default();
        model.open(&path);
        model.edit("# v2");
        model.save();

        assert!(!model.dirty);
        assert_eq!(model.error, None);
        assert_eq!(fs::read_to_string(&path).unwrap(), "# v2");
        // Saved content matches disk: the poll must not "reload" it.
        assert!(!model.reload_if_changed());
    }

    #[test]
    fn save_failure_sets_error_banner() {
        let mut model = Model::default();
        model.document = Some(Document {
            path: std::env::temp_dir().join("pound-no-such-dir/x.md"),
            source: "# x".to_owned(),
            html: String::new(),
            modified: None,
            crlf: false,
            file_type: FileType::Markdown,
            shape: None,
            grid: None,
            csv_sort: None,
        });
        model.dirty = true;
        model.save();
        assert!(model.dirty, "failed save keeps the unsaved state");
        assert!(model.error.as_deref().unwrap_or("").contains("cannot save"));
    }

    #[test]
    fn export_writes_standalone_html_next_to_the_source() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // The image must exist for the renderer to localize it (and the
        // export to hand back a relative src).
        fs::write(dir.join("pic-export.png"), b"png").unwrap();
        let path = temp_md("export.md", "# Export me\n\n![pic](pic-export.png)");
        let mut model = Model::default();
        model.open(&path);
        let revision = model.revision;

        assert!(model.export_html());
        let target = path.with_extension("html");
        let page = fs::read_to_string(&target).unwrap();
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<h1>Export me</h1>"));
        assert!(page.contains(r#"src="pic-export.png""#), "{page}");
        assert_eq!(model.notice.as_deref(), Some("exported to export.html"));
        assert_eq!(model.error, None);
        // An export is not an edit: never dirty, no revision bump.
        assert!(!model.dirty);
        assert_eq!(model.revision, revision);

        // Editing clears the stale notice (the export no longer matches
        // the buffer) — and unsaved edits still ship in the next export.
        model.edit("# Export me twice\n");
        assert_eq!(model.notice, None);
        assert!(model.export_html());
        assert!(fs::read_to_string(&target).unwrap().contains("twice"));
    }

    #[test]
    fn export_failure_sets_error_banner() {
        // Struct-update syntax, not field reassignment after Default:
        // keeps this test clean under clippy's -D warnings.
        let mut model = Model {
            document: Some(Document {
                path: std::env::temp_dir().join("pound-no-such-dir/x.md"),
                source: "# x".to_owned(),
                html: String::new(),
                modified: None,
                crlf: false,
                file_type: FileType::Markdown,
                shape: None,
                grid: None,
                csv_sort: None,
            }),
            ..Model::default()
        };
        assert!(model.export_html());
        assert_eq!(model.notice, None);
        assert!(model
            .error
            .as_deref()
            .unwrap_or("")
            .contains("cannot export"));
    }

    #[test]
    fn export_is_markdown_only() {
        let dir = std::env::temp_dir().join(format!("pound-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let txt = dir.join("export-plain.txt");
        fs::write(&txt, "just text").unwrap();
        let csv = dir.join("export-table.csv");
        fs::write(&csv, "a\n1\n").unwrap();
        let mut model = Model::default();
        model.open(&txt);
        assert!(!model.export_html());
        assert_eq!(model.notice, None);
        model.open(&csv);
        assert!(!model.export_html());
        assert!(!dir.join("export-table.html").exists());
    }

    #[test]
    fn crlf_line_endings_survive_edit_and_save() {
        let path = temp_md("crlf.md", "a\r\nb\r\n");
        let mut model = Model::default();
        model.open(&path);
        assert!(model.document.as_ref().unwrap().crlf);

        // The DOM reports the editor buffer with LF only.
        model.edit("a\nb\nc");
        model.save();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\r\nb\r\nc");
    }

    #[test]
    fn unsaved_edits_pause_disk_reload_and_mark_stale() {
        let path = temp_md("conflict.md", "# disk v1");
        let mut model = Model::default();
        model.open(&path);
        model.edit("# my edit");

        // The file changes on disk under the unsaved edit.
        fs::write(&path, "# disk v2").unwrap();
        bump_mtime(&path);

        assert!(!model.reload_if_changed());
        assert!(model.stale);
        assert!(model.document.as_ref().unwrap().source.contains("my edit"));

        // Saving resolves the conflict by writing the buffer (last writer
        // wins) and clears the notice.
        model.save();
        assert!(!model.stale);
        assert!(fs::read_to_string(&path).unwrap().contains("my edit"));
    }

    #[test]
    fn identical_disk_touch_does_not_reload() {
        let path = temp_md("touch.md", "# v1");
        let mut model = Model::default();
        model.open(&path);
        let revision = model.revision;

        // Same bytes, newer mtime: not a reload.
        fs::write(&path, "# v1").unwrap();
        bump_mtime(&path);

        assert!(!model.reload_if_changed());
        assert_eq!(model.revision, revision);
    }

    #[test]
    fn reloads_when_mtime_changes() {
        let path = temp_md("watch.md", "# v1");
        let mut model = Model::default();
        model.open(&path);
        assert!(!model.reload_if_changed());

        fs::write(&path, "# v2").unwrap();
        // Bump mtime explicitly (some filesystems share the timestamp).
        let later = SystemTime::now() + Duration::from_secs(5);
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(later).unwrap();
        drop(f);

        assert!(model.reload_if_changed());
        let doc = model.document.as_ref().unwrap();
        assert!(doc.html.contains("<h1>v2</h1>"));
        assert_eq!(model.revision, 2);
    }
}
