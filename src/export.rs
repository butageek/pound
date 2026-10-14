//! Export: markdown source → a standalone HTML file (rendering-input
//! layer, like `markdown.rs`/`csv.rs`: pure string work, headless-testable).
//!
//! The export is the reader's own rendering, packaged to travel: the
//! same ammonia-sanitized, syntax-highlighted HTML the preview shows,
//! wrapped in a plain HTML page with its own embedded stylesheet —
//! Solarized Light by default, One Dark when the system prefers dark,
//! mirroring the app's palettes. Local images come back as relative
//! paths (the export lands next to the source file, so they resolve),
//! and nothing references Pound: the file renders identically in any
//! browser with no network, no assets, no scripts.

use std::path::{Component, Path};

use crate::markdown;

/// The marker the renderer emits for local images (see
/// `markdown::localize_image`): `src="poundimg://<percent-encoded
/// absolute path>"`. ammonia always double-quotes attributes, so this
/// prefix pins the rewrite to real src values.
const IMG_SRC: &str = r#"src="poundimg://"#;

/// Build the standalone HTML document for `source`, with images
/// resolved against `base_dir` — the document's directory, which is
/// also where the export lands, so the rewritten relative paths keep
/// working. `title` is the page `<title>` (usually the file's stem).
pub fn standalone_html(source: &str, base_dir: &Path, title: &str) -> String {
    // A fresh render of the live buffer (not the cached preview): the
    // export is exactly what the reader shows, unsaved edits included.
    let body = delocalize_images(&markdown::to_html(source, base_dir), base_dir);
    format!(
        "<!doctype html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"generator\" content=\"Pound\">\n\
<title>{}</title>\n\
<style>\n{STYLES}</style>\n\
</head>\n\
<body>\n\
{body}\n\
</body>\n\
</html>\n",
        escape_html(title),
    )
}

/// Rewrite every `poundimg://` image src back into a path relative to
/// `base_dir` (percent-encoded for the URL). The renderer had
/// canonicalized them to absolute protocol URLs for the app's image
/// server; an export sitting in `base_dir` wants the same relative
/// reference the markdown source made.
fn delocalize_images(html: &str, base_dir: &Path) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find(IMG_SRC) {
        let url_start = start + IMG_SRC.len();
        // The URL runs to the closing quote (ammonia's attribute
        // values cannot contain a raw one). A missing close means the
        // stream is malformed: keep the rest verbatim, never eat it.
        let Some(len) = rest[url_start..].find('"') else {
            break;
        };
        let encoded = &rest[url_start..url_start + len];
        let decoded = markdown::percent_decode(encoded).unwrap_or_else(|| encoded.to_owned());
        out.push_str(&rest[..start]);
        out.push_str("src=\"");
        out.push_str(&encode_url_path(&relative_to(
            Path::new(&decoded),
            base_dir,
        )));
        rest = &rest[url_start + len..]; // keeps the closing quote
    }
    out.push_str(rest);
    out
}

/// `absolute` expressed relative to `base` with `/` separators
/// (`C:\docs\img.png` under `C:\docs` → `img.png`, `C:\img.png` under
/// `C:\docs` → `../img.png`). When the two share no prefix (different
/// drives), falls back to the absolute path — a relative link would be
/// wrong, an absolute one merely less portable.
fn relative_to(absolute: &Path, base: &Path) -> String {
    let absolute = absolute.components().collect::<Vec<Component>>();
    let base = base.components().collect::<Vec<Component>>();
    let mut shared = 0;
    while shared < absolute.len() && shared < base.len() && absolute[shared] == base[shared] {
        shared += 1;
    }
    if shared == 0 {
        return forward_slashed(absolute);
    }
    let mut parts: Vec<String> = Vec::new();
    parts.resize(base.len() - shared, "..".to_owned());
    parts.extend(
        absolute[shared..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// Percent-encode a path for a URL, keeping `/` intact (it separates
/// segments; encoding it would make one literal `%2F` name) — the
/// same unreserved set as the renderer's encoder, plus that slash.
fn encode_url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A path in URL form: `/` separators whatever the host uses.
fn forward_slashed(components: Vec<Component>) -> String {
    components
        .iter()
        .map(|c| c.as_os_str().to_string_lossy().replace('\\', "/"))
        .collect::<Vec<String>>()
        .join("/")
}

/// Escape text for HTML content/attribute contexts (the title is a
/// file name — free-form input like everything else in a file's
/// metadata).
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The exported page's stylesheet: the reader's markdown styles,
/// scoped to plain elements, with the app's light (Solarized Light)
/// and dark (One Dark) palettes switched by the system preference —
/// the same colors the reader would pick, no assets required.
const STYLES: &str = r#"  :root {
    color-scheme: light dark;
    --bg: #fdf6e3; --text: #586e75; --muted: #93a1a1; --border: #eee8d5;
    --link: #268bd2; --code-bg: rgba(147,161,161,0.18); --code-block-bg: #f3ecda;
    --th-bg: #eee8d5;
    --syn-keyword: #859900; --syn-string: #2aa198; --syn-comment: #93a1a1;
    --syn-constant: #d33682; --syn-function: #268bd2; --syn-type: #b58900;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #282c34; --text: #abb2bf; --muted: #7f8792; --border: #3e4451;
      --link: #61afef; --code-bg: rgba(171,178,191,0.15); --code-block-bg: #2c313a;
      --th-bg: #2c313a;
      --syn-keyword: #c678dd; --syn-string: #98c379; --syn-comment: #5c6370;
      --syn-constant: #d19a66; --syn-function: #61afef; --syn-type: #e5c07b;
    }
  }
  html { background: var(--bg); }
  body {
    margin: 0 auto; padding: 28px 24px 120px; max-width: 920px;
    background: var(--bg); color: var(--text);
    font-family: "Segoe UI", "Segoe UI Variable Text", system-ui, sans-serif;
    font-size: 15.5px; line-height: 1.65;
  }
  h1, h2, h3, h4, h5, h6 { font-weight: 600; line-height: 1.25; margin: 26px 0 14px; }
  h1 { font-size: 1.9em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
  h2 { font-size: 1.45em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
  h3 { font-size: 1.22em; } h4 { font-size: 1.05em; }
  h5 { font-size: 0.95em; } h6 { font-size: 0.87em; color: var(--muted); }
  p { margin: 0 0 16px; }
  a { color: var(--link); text-decoration: none; }
  a:hover { text-decoration: underline; }
  code {
    background: var(--code-bg); border-radius: 6px; padding: .2em .4em;
    font-family: Consolas, "Cascadia Mono", monospace; font-size: 85%;
  }
  pre {
    background: var(--code-block-bg); border-radius: 6px; padding: 14px 16px;
    margin: 0 0 16px; overflow: auto; line-height: 1.45;
  }
  pre code { background: none; padding: 0; }
  /* Syntax highlighting: syntect's classed spans, colored per palette
     exactly like the reader pane (no colors are baked into the HTML). */
  pre .type { color: var(--syn-type); }
  pre .keyword, pre .storage { color: var(--syn-keyword); }
  pre .string { color: var(--syn-string); }
  pre .comment { color: var(--syn-comment); font-style: italic; }
  pre .constant { color: var(--syn-constant); }
  pre .entity.function,
  pre .support.function,
  pre .variable.function { color: var(--syn-function); }
  blockquote {
    margin: 0 0 16px; padding: 0 1em; color: var(--muted);
    border-left: .25em solid var(--border);
  }
  table { border-collapse: collapse; margin: 0 0 16px; }
  th, td { border: 1px solid var(--border); padding: 6px 13px; }
  th { background: var(--th-bg); font-weight: 600; }
  img { max-width: 100%; }
  hr { border: 0; border-top: 1px solid var(--border); margin: 24px 0; }
  ul, ol { padding-left: 2em; margin: 0 0 16px; }
  li { margin: .25em 0; }
  li:has(> input[type="checkbox"]) { list-style: none; margin-left: -1.2em; }
  input[type="checkbox"] { margin: 0 .6em 0 0; vertical-align: middle; }
  kbd {
    font: 85% Consolas, "Cascadia Mono", monospace; border: 1px solid var(--border);
    border-radius: 4px; padding: 1px 5px; background: var(--code-block-bg);
  }
  del { color: var(--muted); }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A fresh subdir per test: the module's tests share the process
    /// temp dir and run in parallel.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("pound-test-{}", std::process::id()))
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn exports_a_self_contained_document() {
        let dir = temp_dir("basic");
        std::fs::write(dir.join("notes.md"), "# Title\n\nbody **bold**\n").unwrap();
        let html = standalone_html("# Title\n\nbody **bold**\n", &dir, "notes");

        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<title>notes</title>"), "{html}");
        assert!(html.contains("<meta charset=\"utf-8\">"), "{html}");
        assert!(html.contains("<h1>Title</h1>"), "{html}");
        assert!(html.contains("<strong>bold</strong>"), "{html}");
        // Both palettes ship: the file follows the system preference.
        assert!(html.contains("#fdf6e3"), "light palette: {html}");
        assert!(html.contains("#282c34"), "dark palette: {html}");
    }

    #[test]
    fn titles_are_escaped() {
        let html = standalone_html("# x", Path::new("/tmp"), "a<b> & \"quotes\"");
        assert!(
            html.contains("<title>a&lt;b&gt; &amp; &quot;quotes&quot;</title>"),
            "{html}"
        );
    }

    #[test]
    fn local_images_become_relative_srcs() {
        let dir = temp_dir("images");
        std::fs::create_dir_all(dir.join("imgs")).unwrap();
        std::fs::write(dir.join("imgs/pic.png"), b"png").unwrap();
        let source = "![pic](imgs/pic.png)";
        let html = standalone_html(source, &dir, "notes");

        assert!(
            html.contains(r#"src="imgs/pic.png""#),
            "relative src expected: {html}"
        );
        assert!(
            !html.contains("poundimg"),
            "protocol URLs must not ship: {html}"
        );
    }

    #[test]
    fn images_above_the_document_dir_go_up() {
        let dir = temp_dir("up");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("pic.png"), b"png").unwrap();
        let html = standalone_html("![pic](../pic.png)", &sub, "notes");

        assert!(
            html.contains(r#"src="../pic.png""#),
            "parent-relative src expected: {html}"
        );
    }

    #[test]
    fn special_characters_are_percent_encoded() {
        let dir = temp_dir("encoding");
        std::fs::write(dir.join("my pic & shot.png"), b"png").unwrap();
        let html = standalone_html("![x](my%20pic%20%26%20shot.png)", &dir, "notes");

        // The URL stays valid (and the attribute stays quotable) while
        // the readable characters pass through.
        assert!(
            html.contains(r#"src="my%20pic%20%26%20shot.png""#),
            "{html}"
        );
    }

    #[test]
    fn exported_content_is_sanitized() {
        let dir = temp_dir("sanitize");
        let source = "hi <script>alert(1)</script> <b onclick=\"x()\">bold</b>";
        let html = standalone_html(source, &dir, "notes");

        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("onclick"), "{html}");
        assert!(html.contains("<b>bold</b>"), "{html}");
    }

    #[test]
    fn code_blocks_keep_their_highlight_classes() {
        let dir = temp_dir("highlight");
        let html = standalone_html("```rust\nfn main() {}\n```\n", &dir, "notes");

        assert!(html.contains("language-rust"), "{html}");
        assert!(html.contains("storage type"), "{html}");
        assert!(html.contains("--syn-keyword"), "token colors ship: {html}");
    }
}

#[cfg(test)]
mod dump_tests {
    use super::*;
    #[test]
    #[ignore = "manual check: exports the repo's sample.md for browser inspection"]
    fn dump_exported_sample() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(root.join("sample.md")).expect("sample.md");
        let html = standalone_html(&source, root, "sample");
        std::fs::write(std::env::temp_dir().join("pound-export-sample.html"), html).unwrap();
    }
}
