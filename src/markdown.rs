//! Markdown → HTML (Model layer).
//!
//! The rendered pane is a WebView2 showing HTML/CSS, which gives the same
//! rendering quality as VSCode's markdown preview (also a browser engine).
//! This module turns the markdown source into that HTML:
//!
//! - pulldown-cmark generates the HTML (tables, task lists, strikethrough…)
//! - local image `src`s are rewritten to the `poundimg://` custom protocol
//!   (served from disk by the view layer, like ColaMD's `file://` mapping)
//! - the result is sanitized with ammonia: markdown files can contain raw
//!   HTML, and file content must never execute in the reader (borrowed
//!   lesson from ColaMD's architecture notes).

use std::path::Path;

use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};

/// Render markdown source to sanitized HTML, resolving image references
/// against `base_dir` (the document's directory).
pub fn to_html(source: &str, base_dir: &Path) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(source, options).map(|event| match event {
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            let resolved = localize_image(&dest_url, base_dir);
            Event::Start(Tag::Image {
                link_type,
                dest_url: CowStr::from(resolved),
                title,
                id,
            })
        }
        other => other,
    });

    let mut raw = String::new();
    html::push_html(&mut raw, parser);
    sanitize(&raw)
}

/// Rewrite relative image URLs to `poundimg://<absolute path>`; leave
/// remote/data URLs untouched.
fn localize_image(url: &str, base_dir: &Path) -> String {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("data:")
        || lower.starts_with("poundimg://")
    {
        return url.to_owned();
    }
    let joined = base_dir.join(url);
    match joined.canonicalize() {
        Ok(absolute) => format!(
            "poundimg://{}",
            percent_encode(&absolute.display().to_string())
        ),
        // Missing file: keep the original src so it shows as a broken image.
        Err(_) => url.to_owned(),
    }
}

/// Percent-encode everything outside the URI unreserved set so spaces,
/// `#`, `?` etc. in file paths survive the URL round-trip.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b':'
            | b'\\'
            | b'/' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Decode a percent-encoded `poundimg` URI body back into a filesystem path.
pub fn decode_poundimg_uri(uri: &str) -> Option<String> {
    let body = uri.strip_prefix("poundimg://")?;
    let bytes = body.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn hex(byte: Option<&u8>) -> Option<u8> {
    let b = *byte?;
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Keep the markdown-oriented tags pulldown emits (including the task-list
/// `<input type="checkbox">`), strip scripts, event handlers and styling
/// from raw HTML embedded in the file.
fn sanitize(raw: &str) -> String {
    let tags = [
        "a",
        "abbr",
        "b",
        "bdi",
        "blockquote",
        "br",
        "caption",
        "code",
        "dd",
        "del",
        "details",
        "div",
        "dl",
        "dt",
        "em",
        "figcaption",
        "figure",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "i",
        "img",
        "input",
        "ins",
        "kbd",
        "li",
        "mark",
        "ol",
        "p",
        "pre",
        "q",
        "s",
        "samp",
        "small",
        "span",
        "strike",
        "strong",
        "sub",
        "summary",
        "sup",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "tr",
        "u",
        "ul",
        "var",
    ]
    .into_iter()
    .collect::<std::collections::HashSet<&str>>();

    ammonia::Builder::new()
        .tags(tags)
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        // pulldown leaves remote images as-is and we rewrite local ones to
        // poundimg:// — both must survive the sanitizer's URL filter.
        .add_url_schemes(["poundimg", "data"])
        .url_relative(ammonia::UrlRelative::PassThrough)
        .clean(raw)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/docs")
    }

    #[test]
    #[ignore = "manual preview helper: renders the repo's sample.md for browser testing"]
    fn dump_rendered_sample() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(root.join("sample.md")).expect("sample.md");
        let html = to_html(&source, root);
        std::fs::write(std::env::temp_dir().join("pound-sample-content.html"), html).unwrap();
        std::fs::write(std::env::temp_dir().join("pound-sample-source.txt"), source).unwrap();
    }

    #[test]
    fn headings_and_emphasis() {
        let html = to_html("# Title\n\nHello **bold** and *it* and `code`", &dir());
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("<em>it</em>"));
        assert!(html.contains("<code>code</code>"));
    }

    #[test]
    fn tables_render_as_real_tables() {
        let html = to_html("| a | b |\n|---|---|\n| 1 | 2 |\n", &dir());
        assert!(html.contains("<table>"));
        assert!(html.contains("<th>"));
        assert!(html.contains("<td>"));
    }

    #[test]
    fn task_lists_get_checkboxes() {
        let html = to_html("- [x] done\n- [ ] todo\n", &dir());
        assert!(html.contains(r#"type="checkbox""#), "html was: {html}");
        assert!(html.contains("checked"), "html was: {html}");
    }

    #[test]
    fn local_images_are_rewritten_to_the_custom_protocol() {
        // A missing file keeps its original src…
        let html = to_html("![missing](nope/never-exists.png)", &dir());
        assert!(html.contains("nope/never-exists.png"));
        // …while an existing file resolves to an absolute poundimg:// URL.
        let tmp = std::env::temp_dir().join(format!("pound-img-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("pic.txt"), b"x").unwrap();
        let html = to_html("![pic](pic.txt)", &tmp);
        let expected = format!(
            "poundimg://{}",
            percent_encode(
                &tmp.join("pic.txt")
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
            )
        );
        assert!(html.contains(&expected), "html was: {html}");
    }

    #[test]
    fn remote_images_are_untouched() {
        let html = to_html("![x](https://example.com/a%20b.png)", &dir());
        assert!(html.contains("https://example.com/a%20b.png"));
    }

    #[test]
    fn raw_html_scripts_are_stripped() {
        let html = to_html(
            "hello <script>alert(1)</script> and <b onclick=\"x()\">bold</b>",
            &dir(),
        );
        assert!(!html.contains("script"));
        assert!(!html.contains("onclick"));
        assert!(html.contains("<b>bold</b>"));
    }

    #[test]
    fn poundimg_uri_round_trip() {
        let path = r"C:\Users\hendry.chou\My Docs\img #1.png";
        let encoded = percent_encode(path);
        assert_eq!(
            decode_poundimg_uri(&format!("poundimg://{encoded}")).unwrap(),
            path
        );
    }
}
