//! Config-file rendering (rendering-input layer, like `markdown.rs` /
//! `csv.rs`: pure string work, headless-testable). A whole TOML or YAML
//! file becomes one sanitized, syntax-highlighted block for the
//! rendered pane — TOML through a small hand-rolled tokenizer (syntect's
//! default syntax set ships no TOML; the grammar is a dozen rules), YAML
//! through syntect like fenced blocks in markdown. Either way the spans
//! carry the shell's `--syn-*` token classes — no colors are baked in,
//! every palette themes them.
//!
//! When highlighting declines (unknown language, or syntect refuses a
//! line), the plain escaped block ships instead — rendering failures
//! fall back to source, never eat content.

use std::path::Path;

use crate::markdown;

/// The syntect language token a path's extension maps to — also the
/// `FileType::Code` payload. `None` for everything else (plain editor
/// pane, no rendering).
pub fn language_for(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}

/// Highlight any language the app knows: TOML through the local
/// tokenizer, everything else through syntect. Also the entry point
/// markdown's fenced blocks use — a ```toml fence in a .md file gets
/// the same colors as an open .toml file.
pub fn highlight(code: &str, language: &str) -> Option<String> {
    match language {
        "toml" => Some(highlight_toml(code)),
        _ => markdown::highlighted(code, language),
    }
}

/// Render `source` as one highlighted block. The wrapper div carries
/// the whole file's line range (`1..N`) — one block, so the split view's
/// scroll sync interpolates across the file.
pub fn to_html(source: &str, language: &str) -> String {
    let lines = source.lines().count().max(1);
    let block = highlight(source, language).unwrap_or_else(|| plain_block(source));
    let mut raw = format!(r#"<div data-line-start="1" data-line-end="{lines}">"#);
    raw.push_str(&block);
    raw.push_str("</div>");
    markdown::sanitize(&raw)
}

/// The no-highlighting fallback: escaped text in a bare code block.
fn plain_block(source: &str) -> String {
    format!("<pre><code>{}</code></pre>", escape_html(source))
}

/// Highlight TOML. The classes are the shell CSS's known tokens:
/// comments → `comment`, strings → `string`, numbers/booleans/datetimes
/// → `constant`, keys and table headers → `type`; punctuation stays
/// plain. A lenient scanner: malformed input just gets imperfect
/// colors, never mangled text (spans wrap escaped source verbatim).
fn highlight_toml(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let n = chars.len();
    let mut out = String::from("<pre><code class=\"language-toml\">");
    let mut plain = String::new();
    // Open containers (`[` array / `{` inline table): after a `,` the
    // next token is a value inside arrays but a key inside inline
    // tables, and a bare `[` at statement level is a table header —
    // the stack and `expect_value` tell those apart.
    let mut stack: Vec<char> = Vec::new();
    let mut expect_value = false;
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '#' => {
                let (span, next) = until_newline(&chars, i);
                flush_plain(&mut out, &mut plain);
                push_span(&mut out, "comment", span);
                i = next;
            }
            '"' | '\'' => {
                let (span, next) = read_string(&chars, i);
                flush_plain(&mut out, &mut plain);
                push_span(&mut out, "string", span);
                i = next;
                expect_value = false;
            }
            // A bracket before any value on the statement is a table
            // header (`[a.b]` / `[[a.b]]`) — brackets inside stay in
            // the span; quoted `]` inside headers is vanishingly rare
            // and merely shifts the color boundary.
            '[' if stack.is_empty() && !expect_value => {
                let start = i;
                let double = chars.get(i + 1) == Some(&'[');
                i += usize::from(double) + 1;
                while i < n && chars[i] != '\n' {
                    if chars[i] == ']' {
                        i += 1;
                        if double && chars.get(i) == Some(&']') {
                            i += 1;
                        }
                        break;
                    }
                    i += 1;
                }
                flush_plain(&mut out, &mut plain);
                push_span(&mut out, "type", &chars[start..i]);
            }
            '[' => {
                plain.push('[');
                stack.push('[');
                expect_value = true;
                i += 1;
            }
            '{' => {
                plain.push('{');
                stack.push('{');
                i += 1; // a key follows, not a value
            }
            ']' | '}' => {
                plain.push(c);
                if stack.last() == Some(&c) {
                    stack.pop();
                }
                expect_value = false;
                i += 1;
            }
            ',' => {
                plain.push(',');
                expect_value = stack.last() == Some(&'[');
                i += 1;
            }
            '=' => {
                plain.push('=');
                expect_value = true;
                i += 1;
            }
            // Whitespace: a newline at statement level ends the
            // statement (next token is a key or header again).
            ' ' | '\t' | '\r' | '\n' => {
                plain.push(c);
                if c == '\n' && stack.is_empty() {
                    expect_value = false;
                }
                i += 1;
            }
            // A bare word: a key when an `=` follows (peeked past
            // spaces), a value otherwise. Valid TOML values here are
            // numbers, booleans and datetimes — all constants.
            _ => {
                let start = i;
                while i < n
                    && !matches!(
                        chars[i],
                        ' ' | '\t'
                            | '\r'
                            | '\n'
                            | '#'
                            | ','
                            | '['
                            | ']'
                            | '{'
                            | '}'
                            | '='
                            | '"'
                            | '\''
                    )
                {
                    i += 1;
                }
                let mut peek = i;
                while matches!(chars.get(peek), Some(' ') | Some('\t')) {
                    peek += 1;
                }
                let class = if chars.get(peek) == Some(&'=') {
                    "type"
                } else {
                    "constant"
                };
                flush_plain(&mut out, &mut plain);
                push_span(&mut out, class, &chars[start..i]);
                expect_value = false;
            }
        }
    }
    flush_plain(&mut out, &mut plain);
    out.push_str("</code></pre>");
    out
}

/// A comment's text: from `#` to (not including) the newline.
fn until_newline(chars: &[char], from: usize) -> (&[char], usize) {
    let mut end = from;
    while end < chars.len() && chars[end] != '\n' {
        end += 1;
    }
    (&chars[from..end], end)
}

/// A string from `from` (the opening quote): triple-quoted strings span
/// lines; basic (`"`) strings honor `\` escapes; unterminated strings
/// run to the end of the line (or file, for triples) — lenient, and
/// the text always ships verbatim.
fn read_string(chars: &[char], from: usize) -> (&[char], usize) {
    let quote = chars[from];
    let triple = chars.get(from + 1) == Some(&quote) && chars.get(from + 2) == Some(&quote);
    let mut i = from + usize::from(triple) * 2 + 1;
    loop {
        if triple {
            if i + 2 >= chars.len() {
                return (&chars[from..], chars.len()); // unterminated: to EOF
            }
            if chars[i] == '\\' && quote == '"' {
                i += 2;
                continue;
            }
            if chars[i] == quote && chars[i + 1] == quote && chars[i + 2] == quote {
                return (&chars[from..i + 3], i + 3);
            }
            i += 1;
        } else {
            if i >= chars.len() || chars[i] == '\n' {
                return (&chars[from..i], i); // unterminated: to EOL
            }
            if chars[i] == '\\' && quote == '"' {
                i += 2;
                continue;
            }
            if chars[i] == quote {
                return (&chars[from..i + 1], i + 1);
            }
            i += 1;
        }
    }
}

/// Append a classed span with its text HTML-escaped.
fn push_span(out: &mut String, class: &str, text: &[char]) {
    out.push_str("<span class=\"");
    out.push_str(class);
    out.push_str("\">");
    out.push_str(&escape_html(&text.iter().collect::<String>()));
    out.push_str("</span>");
}

/// Flush buffered plain text (escaped) into the output.
fn flush_plain(out: &mut String, plain: &mut String) {
    out.push_str(&escape_html(plain));
    plain.clear();
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_files_highlight_through_syntect() {
        let html = to_html("key: value\n# note\n", "yaml");
        assert!(
            html.contains(r#"data-line-start="1" data-line-end="2""#),
            "{html}"
        );
        assert!(html.contains("language-yaml"), "{html}");
        assert!(html.contains("<span class="), "classed spans: {html}");
        // Content ships verbatim — nested spans may split it, so match
        // whole tokens.
        assert!(html.contains(">key<"), "{html}");
        assert!(html.contains(">value<"), "{html}");
        assert!(html.contains("note"), "{html}");
    }

    #[test]
    fn toml_tables_keys_values_and_comments() {
        let source = r##"# config
[server]
host = "localhost"
port = 8080
enabled = true
"##;
        let html = to_html(source, "toml");
        assert!(
            html.contains(r#"<span class="comment"># config</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="type">[server]</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<span class="type">host</span>"#), "{html}");
        assert!(
            html.contains(r#"<span class="string">"localhost"</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="constant">8080</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="constant">true</span>"#),
            "{html}"
        );
        // The whole file is one block for the scroll sync.
        assert!(
            html.contains(r#"data-line-start="1" data-line-end="5""#),
            "{html}"
        );
    }

    #[test]
    fn toml_arrays_multiline_strings_and_inline_tables() {
        let source =
            "tags = [\"a\", 'b', 3]\n\ndesc = \"\"\"\nlines \"\"\"\npoint = { x = 1, y = 2 }\n";
        let html = to_html(source, "toml");
        // Values inside arrays keep their classes.
        assert!(
            html.contains(r#"<span class="string">"a"</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="string">'b'</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="constant">3</span>"#),
            "{html}"
        );
        // A triple-quoted string swallows its quotes and newlines.
        assert!(
            html.contains("<span class=\"string\">\"\"\"\nlines \"\"\"</span>"),
            "{html}"
        );
        // Inline-table members are keys, not constants.
        assert!(html.contains(r#"<span class="type">x</span>"#), "{html}");
        assert!(
            html.contains(r#"<span class="constant">1</span>"#),
            "{html}"
        );
    }

    #[test]
    fn toml_text_is_escaped_and_never_eaten() {
        let source = "link = \"<b>&amp;</b>\"\n";
        let html = to_html(source, "toml");
        assert!(html.contains("&lt;b&gt;&amp;amp;&lt;/b&gt;"), "{html}");
        assert!(!html.contains("<b>"), "raw HTML must not survive: {html}");
    }

    #[test]
    fn unterminated_strings_and_rough_input_survive() {
        // Malformed TOML must produce imperfect colors, not lost text.
        let source = "a = \"never closed\nb = 1 = 2\n[unclosed\n";
        let html = to_html(source, "toml");
        for fragment in ["a", "\"never closed", "b", "1", "2", "[unclosed"] {
            assert!(html.contains(fragment), "lost {fragment:?}: {html}");
        }
    }

    #[test]
    fn unknown_languages_fall_back_to_plain() {
        let html = to_html("a < b\n", "notalanguage");
        assert!(
            html.contains("<pre><code>a &lt; b\n</code></pre>"),
            "{html}"
        );
        assert!(!html.contains("<span"), "{html}");
    }

    #[test]
    fn language_for_maps_extensions() {
        let f = |name: &str| language_for(Path::new(name));
        assert_eq!(f("Cargo.toml"), Some("toml"));
        assert_eq!(f("conf.yaml"), Some("yaml"));
        assert_eq!(f("CONF.YML"), Some("yaml")); // case-insensitive
        assert_eq!(f("notes.md"), None);
        assert_eq!(f("noext"), None);
    }
}
