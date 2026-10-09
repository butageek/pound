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
        Ok(Document {
            html: markdown::to_html(&source, absolute.parent().unwrap_or(Path::new("."))),
            path: absolute,
            source,
            modified,
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
    /// Bumped every time the document is (re)loaded; lets the view
    /// invalidate caches such as loaded image textures.
    pub revision: u64,
}

impl Model {
    pub fn open(&mut self, path: &Path) {
        match Document::load(path) {
            Ok(doc) => {
                self.document = Some(doc);
                self.error = None;
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

    /// Reload the document if its modification time changed on disk.
    /// Returns `true` when the document was reloaded.
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
        if doc.modified != Some(modified) {
            self.reload();
            true
        } else {
            false
        }
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
            }
        }
        assert_eq!(doc_with("md").type_label(), "Markdown");
        assert_eq!(doc_with("markdown").type_label(), "Markdown");
        assert_eq!(doc_with("txt").type_label(), "Plain text");
        assert_eq!(doc_with("json").type_label(), "JSON file");
        assert_eq!(doc_with("csv").type_label(), "CSV file");
        assert_eq!(doc_with("").type_label(), "File");
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
