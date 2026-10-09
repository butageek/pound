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
}

impl Document {
    pub fn load(path: &Path) -> Result<Document, String> {
        let absolute = strip_verbatim_prefix(
            path.canonicalize()
                .map_err(|e| format!("cannot open {}: {e}", path.display()))?,
        );
        let source = fs::read_to_string(&absolute)
            .map_err(|e| format!("cannot read {}: {e}", absolute.display()))?;
        let modified = fs::metadata(&absolute).and_then(|m| m.modified()).ok();
        let crlf = source.contains("\r\n");
        Ok(Document {
            html: markdown::to_html(&source, absolute.parent().unwrap_or(Path::new("."))),
            path: absolute,
            source,
            modified,
            crlf,
        })
    }

    /// File name without extension, for the window title.
    pub fn name(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string())
    }

    /// Human-facing file type for the status bar, e.g. `Markdown` or `TXT file`.
    pub fn type_label(&self) -> String {
        let ext = self
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "md" | "markdown" => "Markdown".to_owned(),
            "txt" => "Plain text".to_owned(),
            "" => "File".to_owned(),
            other => format!("{} file", other.to_ascii_uppercase()),
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
        doc.html = markdown::to_html(&doc.source, doc.path.parent().unwrap_or(Path::new(".")));
        self.revision += 1;
        self.dirty = true;
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
        // Pound window) must not reset the editor for no visible reason.
        if fs::read_to_string(&doc.path).is_ok_and(|disk| disk == doc.source) {
            if let Some(doc) = self.document.as_mut() {
                doc.modified = Some(modified);
            }
            return false;
        }
        self.reload();
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
            Document {
                path: PathBuf::from(format!("C:\\notes.{ext}")),
                source: String::new(),
                html: String::new(),
                modified: None,
                crlf: false,
            }
        }
        assert_eq!(doc_with("md").type_label(), "Markdown");
        assert_eq!(doc_with("markdown").type_label(), "Markdown");
        assert_eq!(doc_with("txt").type_label(), "Plain text");
        assert_eq!(doc_with("json").type_label(), "JSON file");
        assert_eq!(doc_with("csv").type_label(), "CSV file");
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
        });
        model.dirty = true;
        model.save();
        assert!(model.dirty, "failed save keeps the unsaved state");
        assert!(model.error.as_deref().unwrap_or("").contains("cannot save"));
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
