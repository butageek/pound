//! Markdown → HTML (Model layer).
//!
//! The rendered pane is a WebView2 showing HTML/CSS, which gives the same
//! rendering quality as VSCode's markdown preview (also a browser engine).
//! This module turns the markdown source into that HTML:
//!
//! - pulldown-cmark generates the HTML (tables, task lists, strikethrough…)
//! - local image `src`s are rewritten to the `poundimg://` custom protocol,
//!   served from disk by the view layer
//! - every top-level block is wrapped in a `<div data-line-start
//!   data-line-end>` carrying its source line range — the view's scroll
//!   sync maps panes through them (VSCode-style)
//! - the result is sanitized with ammonia: markdown files can contain raw
//!   HTML, and file content must never execute in the reader

use std::path::Path;
use std::sync::OnceLock;

use pulldown_cmark::{html, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};
use syntect::html::{ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;

/// Render markdown source to sanitized HTML, resolving image references
/// against `base_dir` (the document's directory).
pub fn to_html(source: &str, base_dir: &Path) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let events = Parser::new_ext(source, options)
        .into_offset_iter()
        .map(|(event, range)| (localize_image_event(event, base_dir), range));

    let mut raw = String::new();
    push_blocks(
        &mut raw,
        highlight_codeblocks(events),
        &line_start_offsets(source),
    );
    sanitize(&raw)
}

/// Rewrite local image `src`s to the `poundimg://` protocol; pass every
/// other event through untouched.
fn localize_image_event<'a>(event: Event<'a>, base_dir: &Path) -> Event<'a> {
    match event {
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
    }
}

/// A code block while being buffered: its original events (the replay
/// path), the accumulated plain text, and the fenced language token.
type BufferedBlock<'a> = (
    Vec<(Event<'a>, std::ops::Range<usize>)>,
    String,
    Option<String>,
);

/// Replace fenced code blocks carrying a known language with syntect
/// classed spans (`<span class="keyword …">`) — the shell CSS colors
/// them per palette, so no colors are baked into the HTML. Blocks the
/// highlighter declines (no language, unknown language, errors) replay
/// their original events and keep pulldown's plain escaped rendering.
fn highlight_codeblocks<'a, I>(
    events: I,
) -> impl Iterator<Item = (Event<'a>, std::ops::Range<usize>)>
where
    I: Iterator<Item = (Event<'a>, std::ops::Range<usize>)>,
{
    let mut block: Option<BufferedBlock<'a>> = None;
    let mut out: Vec<(Event<'a>, std::ops::Range<usize>)> = Vec::new();
    for (event, range) in events {
        let end = range.end;
        match &event {
            Event::Start(Tag::CodeBlock(kind)) if block.is_none() => {
                let language = match kind {
                    CodeBlockKind::Fenced(info) => first_token(info),
                    CodeBlockKind::Indented => None,
                };
                block = Some((vec![(event, range)], String::new(), language));
            }
            Event::End(TagEnd::CodeBlock) if block.is_some() => {
                let (mut events, code, language) = block.take().unwrap();
                events.push((event, range));
                let start = events[0].1.start;
                match language
                    .as_deref()
                    .and_then(|l| crate::code::highlight(&code, l))
                {
                    Some(html) => out.push((Event::Html(CowStr::from(html)), start..end)),
                    None => out.extend(events),
                }
            }
            Event::Text(text) if block.is_some() => {
                let (events, code, _) = block.as_mut().unwrap();
                code.push_str(text);
                events.push((event, range));
            }
            _ if block.is_some() => block.as_mut().unwrap().0.push((event, range)),
            _ => out.push((event, range)),
        }
    }
    out.into_iter()
}

/// The first token of a fence info string ("```rust,ignore" -> "rust");
/// `None` for an empty info string.
fn first_token(info: &str) -> Option<String> {
    info.split([',', ' ', ';', '\t', ':'])
        .find(|token| !token.is_empty())
        .map(str::to_owned)
}

/// Syntect's packed default syntaxes, parsed once per process (a few
/// dozen ms — `warm_syntax_set` moves that off whatever thread would
/// otherwise pay it).
fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Force the syntax set to load now (called from a background thread at
/// startup, so neither the first file load nor a UI-thread render pays
/// the one-time parse).
pub(crate) fn warm_syntax_set() {
    let _ = syntax_set();
}

/// Highlight `code` as `language` into classed spans; `None` when the
/// language is unknown or syntect declines a line — callers then keep
/// the plain rendering, never eating content. Also used by the
/// config-file view (`code.rs`).
pub(crate) fn highlighted(code: &str, language: &str) -> Option<String> {
    let syntaxes = syntax_set();
    let syntax = syntaxes.find_syntax_by_token(language)?;
    let mut generator =
        ClassedHTMLGenerator::new_with_class_style(syntax, syntaxes, ClassStyle::Spaced);
    for line in code.split_inclusive('\n') {
        // The generator wants lines with their newline; a final line
        // without one gets it added (and the added newline trimmed back
        // below, so an unterminated block gains no blank line).
        let terminated;
        let line = if line.ends_with('\n') {
            line
        } else {
            terminated = format!("{line}\n");
            terminated.as_str()
        };
        generator
            .parse_html_for_line_which_includes_newline(line)
            .ok()?;
    }
    let mut html = generator.finalize();
    if !code.ends_with('\n') {
        html.truncate(html.strip_suffix('\n')?.len());
    }
    // The wrapper mirrors pulldown's plain rendering (`<pre><code
    // class="language-x">`) so block styling and the copy button keep
    // working; the token is attribute-escaped before it goes in a class.
    let class = language.replace(['"', '<', '>', '&'], "");
    Some(format!(
        "<pre><code class=\"language-{class}\">{html}</code></pre>"
    ))
}

/// Render the event stream as HTML, wrapping every top-level block in a
/// div carrying its source line range (the view's scroll sync maps panes
/// through these attributes).
fn push_blocks<'a, I>(raw: &mut String, events: I, line_starts: &[usize])
where
    I: Iterator<Item = (Event<'a>, std::ops::Range<usize>)>,
{
    let mut depth = 0usize; // container nesting; 0 = between top-level blocks
    let mut block: Vec<Event<'a>> = Vec::new();
    let mut block_start = 0usize;
    let mut block_end = 0usize;
    for (event, range) in events {
        // A fresh block adopts the range of its first event, whatever
        // kind it is: Start events for markdown constructs, a synthetic
        // Html event for highlighted code blocks (both carry the block's
        // full source span).
        if block.is_empty() {
            block_start = range.start;
            block_end = range.end;
        }
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        block_end = block_end.max(range.end);
        block.push(event);
        if depth == 0 {
            emit_block(raw, &mut block, line_starts, block_start, block_end);
        }
    }
    // Unreachable for balanced streams (pulldown always pairs Start/End);
    // kept so malformed input can never silently drop trailing content.
    if !block.is_empty() {
        emit_block(raw, &mut block, line_starts, block_start, block_end);
    }
}

fn emit_block(
    raw: &mut String,
    block: &mut Vec<Event>,
    line_starts: &[usize],
    start: usize,
    end: usize,
) {
    let first = line_of(line_starts, start);
    let last = line_of(line_starts, end.saturating_sub(1).max(start));
    raw.push_str(&format!(
        r#"<div data-line-start="{first}" data-line-end="{last}">"#
    ));
    html::push_html(raw, block.drain(..));
    raw.push_str("</div>");
}

/// Byte offset where each 1-based line starts (line 1 = offset 0).
fn line_start_offsets(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        source
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i + 1),
    );
    starts
}

/// 1-based number of the line containing byte offset `byte`.
fn line_of(starts: &[usize], byte: usize) -> usize {
    starts.partition_point(|&s| s <= byte)
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

/// Percent-encode everything outside the URI unreserved set. The path is
/// carried as one opaque percent-encoded blob: Windows canonical paths
/// (`\\\\?\\C:\\…`) would otherwise put the drive colon inside the URL
/// authority, which fails URL parsing and makes ammonia strip the src.
fn percent_encode(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Decode a percent-encoded `poundimg` URI body back into a filesystem path.
pub fn decode_poundimg_uri(uri: &str) -> Option<String> {
    percent_decode(uri.strip_prefix("poundimg://")?)
}

/// Percent-decode a URL body (`%20` -> space). Invalid escapes pass
/// through unchanged; input that is not valid UTF-8 after decoding
/// yields `None` so callers can fall back.
pub fn percent_decode(body: &str) -> Option<String> {
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
/// from raw HTML embedded in the file. Also the sanitizer for the
/// config-file view (`code.rs`).
pub(crate) fn sanitize(raw: &str) -> String {
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
        // Highlighting emits `<span class="keyword …">` inside code
        // blocks; the class is inert (colors come from our CSS) and only
        // ever cosmetic.
        .add_tag_attributes("span", ["class"])
        .add_tag_attributes("code", ["class"])
        // Ours: block line annotations for scroll sync (numbers only, read
        // back by the shell — inert even if a file ships its own data-*
        // attributes, since scripts never execute).
        .add_generic_attribute_prefixes(["data-"])
        // pulldown leaves remote images as-is and we rewrite local ones to
        // poundimg:// — both must survive the sanitizer's URL filter.
        // `file:` link hrefs survive too: the host opens them in a new
        // Pound window (see the link ipc), never a navigation.
        .add_url_schemes(["poundimg", "data", "file"])
        .url_relative(ammonia::UrlRelative::PassThrough)
        .clean(raw)
        .to_string()
}

/// Performance smoke test for the render pipeline (kept ignored, like
/// the dump_* helpers): run with
/// `cargo test --release perf_render_paths -- --ignored --nocapture`
/// and compare before/after when touching the renderers. Rendering runs
/// on a background worker in the app, but throughput still bounds how
/// quickly loads and debounced edits land.
#[cfg(test)]
mod perf_render_paths {
    use super::*;
    use std::time::Instant;

    fn repeated(kb: usize, unit: &str) -> String {
        let mut s = String::new();
        for _ in 0..(kb * 1024 / unit.len()) {
            s.push_str(unit);
        }
        s
    }

    #[test]
    #[ignore = "benchmark helper: prints timings, asserts nothing"]
    fn perf_render_paths() {
        let dir = Path::new("/tmp");
        // One-time warm-up (the render worker pays this at startup).
        let t = Instant::now();
        warm_syntax_set();
        println!("syntax set warm-up: {:?}", t.elapsed());

        let md_unit =
            "The quick brown fox jumps over the lazy dog. *em* `code` [l](x.md) — two.\n\n\
                       ```rust\nfn main() {\n    let x = 42;\n}\n```\n\n";
        let toml_unit =
            "# comment\n[server]\nhost = \"localhost\"  # inline\nport = 8080\nenabled = true\n\n";
        let yaml_unit = "key: value\n# comment\nnested:\n  - a\n  - b\n";
        for (name, unit, lang) in [
            ("markdown", md_unit, None),
            ("toml", toml_unit, Some("toml")),
            ("yaml", yaml_unit, Some("yaml")),
        ] {
            for kb in [50, 200, 1000] {
                let source = repeated(kb, unit);
                let t = Instant::now();
                let html = match lang {
                    Some(lang) => crate::code::to_html(&source, lang),
                    None => to_html(&source, dir),
                };
                println!(
                    "{name:>8} render {:>4} KB -> {} KB: {:?}",
                    source.len() / 1024,
                    html.len() / 1024,
                    t.elapsed()
                );
            }
        }
    }
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
    fn blocks_carry_source_line_numbers() {
        let source = "# Title\n\npara one\ncontinues\n\n```rust\nfn main() {}\n```\n";
        let html = to_html(source, &dir());
        assert!(
            html.contains(r#"<div data-line-start="1" data-line-end="1"><h1>Title</h1>"#),
            "html was: {html}"
        );
        assert!(
            html.contains(r#"data-line-start="3" data-line-end="4""#),
            "html was: {html}"
        );
        assert!(
            html.contains(r#"data-line-start="6" data-line-end="8""#),
            "html was: {html}"
        );
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

/// Highlighting tests (kept in their own module: they exercise the
/// syntect integration, not the pure pulldown pipeline above).
#[cfg(test)]
mod highlighting {
    use super::*;

    #[test]
    fn fenced_code_blocks_get_classed_highlights() {
        let html = to_html(
            "```rust\nfn main() { let x = 42; }\n```\n",
            Path::new("/tmp"),
        );
        assert!(html.contains(r#"<code class="language-rust">"#), "{html}");
        assert!(
            html.contains("storage type"),
            "fn is a keyword span: {html}"
        );
        assert!(
            html.contains("entity name function"),
            "fn name span: {html}"
        );
        assert!(html.contains("constant numeric"), "number span: {html}");
    }

    #[test]
    fn highlighted_code_is_still_sanitized() {
        // Angles in code must arrive escaped — highlighted or not, file
        // content is never raw HTML. (The tokens sit in separate spans,
        // so `&lt;` and `script` are not contiguous.)
        let html = to_html(
            "```html\n<script>alert(1)</script>\n```\n",
            Path::new("/tmp"),
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(html.contains("&lt;"), "the angle is escaped: {html}");
        assert!(
            html.contains(">script<"),
            "script is inert span text: {html}"
        );
    }

    #[test]
    fn toml_fences_use_the_local_tokenizer() {
        // syntect ships no TOML; the code.rs tokenizer serves fences too,
        // so a ```toml example in a .md file matches an open .toml file.
        let html = to_html("```toml\n[server]\nport = 1\n```\n", Path::new("/tmp"));
        assert!(html.contains("language-toml"), "{html}");
        assert!(
            html.contains(r#"<span class="type">[server]</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="constant">1</span>"#),
            "{html}"
        );
    }

    #[test]
    fn unknown_language_and_plain_fences_render_escaped() {
        // Unknown fences keep the plain path: escaped text, no spans
        // (pulldown still tags the class; harmless and pre-existing).
        let html = to_html(
            "```notalanguage\nx < 42\n```\n\n    indented < code\n",
            Path::new("/tmp"),
        );
        assert!(html.contains("&lt; 42"), "{html}");
        assert!(html.contains("&lt; code"), "{html}");
        assert!(!html.contains("<span"), "{html}");
    }

    #[test]
    fn fence_info_suffixes_are_ignored() {
        let html = to_html("```rust,ignore\nfn f() {}\n```\n", Path::new("/tmp"));
        assert!(html.contains(r#"<code class="language-rust">"#), "{html}");
    }

    #[test]
    fn file_link_hrefs_survive_sanitization() {
        // The host opens file: links in a new window; ammonia must keep
        // the href (its default scheme filter would strip it).
        let html = to_html("[doc](file:///C:/gone%20doc.md)\n", Path::new("/tmp"));
        assert!(
            html.contains(r#"href="file:///C:/gone%20doc.md""#),
            "{html}"
        );
    }
}
