//! The JS bridge: the wire protocol between the shell document and the
//! host. Both directions live here so the format is headless-testable
//! (the GUI itself is Windows-only).
//!
//! - JS -> Rust: the shell's scripts post short line-oriented messages
//!   over wry's `window.ipc`; [`parse`] turns one into a [`Message`].
//! - Rust -> JS: the view evaluates `pound.setContent(...)` /
//!   `pound.setRendered(...)` etc., with every string encoded by
//!   [`json_str`] (quoting only — ammonia handles untrusted content).
//!
//! `edit` payloads carry the whole editor buffer, `link` payloads a raw
//! href, and `open-export` payloads the exported file's path, so those
//! variants take the entire remainder of the message; everything else is
//! a host-controlled word.

/// One message posted by the shell over `window.ipc`.
#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    /// The shell's scripts parsed and `pound.setContent` is callable. The
    /// host's eager initial push races WebView2's document commit, so the
    /// shell announces itself and the host re-pushes (see `view::run`).
    Ready,
    /// Ctrl+S / the Save button.
    Save,
    /// Ctrl+E / the Export HTML button: write the rendered document as a
    /// standalone HTML file next to its source (markdown documents only).
    ExportHtml,
    /// The export toast's Open: launch the written file in the system
    /// browser. The payload is the target path the host itself reported
    /// in the toast push.
    OpenExport { path: String },
    /// The close prompt's Save: save, then exit (via the updater when one
    /// is pending).
    SaveAndExit,
    /// The close prompt's Don't save: exit now.
    Exit,
    /// The update toast's Update & restart.
    Update,
    /// The close prompt's Cancel (clears a pending update).
    CancelExit,
    /// A settings-page change: validated and persisted by the host (the
    /// shell already applied it locally). The value may contain spaces
    /// (font family names are free-form) and may be empty (back to the
    /// default stacks).
    Setting { name: String, value: String },
    /// The editor's Paste: WebView2 cannot read the clipboard from JS
    /// without a permission prompt, so the host reads it (Win32) and
    /// replies via `pound.setClipboardText`.
    ClipboardRead,
    /// A link click in the rendered pane. The shell intercepts clicks and
    /// posts the raw href because the browser would resolve it against
    /// `about:blank` and mangle it before any navigation handler could
    /// see it.
    Link { href: String },
    /// The (debounced) editor buffer changed.
    Edit { text: String },
    /// A CSV table header click: re-render sorted by that column (the
    /// same column toggles the direction).
    CsvSort { column: usize },
}

/// Parse one ipc message body. Returns `None` for anything the host does
/// not recognize — such messages are ignored, same as the if/else chain
/// this parser replaced.
pub fn parse(body: &str) -> Option<Message> {
    // The payload-carrying kinds are checked first: their payloads may
    // contain anything (newlines included), so they take the whole rest.
    if let Some(text) = body.strip_prefix("edit\n") {
        return Some(Message::Edit {
            text: text.to_owned(),
        });
    }
    if let Some(setting) = body.strip_prefix("setting\n") {
        // "name value" — split_once keeps spaces (and only spaces) in the
        // value; a missing space is not a message.
        let (name, value) = setting.split_once(' ')?;
        return Some(Message::Setting {
            name: name.to_owned(),
            value: value.to_owned(),
        });
    }
    if let Some(href) = body.strip_prefix("link\n") {
        return Some(Message::Link {
            href: href.to_owned(),
        });
    }
    if let Some(path) = body.strip_prefix("open-export\n") {
        return Some(Message::OpenExport {
            path: path.to_owned(),
        });
    }
    if let Some(column) = body.strip_prefix("csv-sort\n") {
        // A bare usize; anything else (negative, text, empty) is not a
        // message and is ignored.
        return column
            .parse()
            .ok()
            .map(|column| Message::CsvSort { column });
    }
    match body {
        "ready" => Some(Message::Ready),
        "save" => Some(Message::Save),
        "export-html" => Some(Message::ExportHtml),
        "save-and-exit" => Some(Message::SaveAndExit),
        "exit" => Some(Message::Exit),
        "update" => Some(Message::Update),
        "cancel-exit" => Some(Message::CancelExit),
        "clipboard-read" => Some(Message::ClipboardRead),
        _ => None,
    }
}

/// Encode a &str as a double-quoted JavaScript string literal (our own
/// HTML is trusted; this is only about quoting, not XSS — ammonia handles
/// that).
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' | '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_words_parse() {
        assert_eq!(parse("ready"), Some(Message::Ready));
        assert_eq!(parse("save"), Some(Message::Save));
        assert_eq!(parse("export-html"), Some(Message::ExportHtml));
        assert_eq!(parse("save-and-exit"), Some(Message::SaveAndExit));
        assert_eq!(parse("exit"), Some(Message::Exit));
        assert_eq!(parse("update"), Some(Message::Update));
        assert_eq!(parse("cancel-exit"), Some(Message::CancelExit));
        assert_eq!(parse("clipboard-read"), Some(Message::ClipboardRead));
    }

    #[test]
    fn unknown_bodies_are_ignored() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("bogus"), None);
        // The word must be the whole body: no trailing newline, no
        // near-miss prefixes.
        assert_eq!(parse("ready\n"), None);
        assert_eq!(parse("savex"), None);
        assert_eq!(parse("READY"), None);
        // Bare prefixes without their payload are not messages either.
        assert_eq!(parse("edit"), None);
        assert_eq!(parse("setting"), None);
        assert_eq!(parse("link"), None);
        assert_eq!(parse("open-export"), None);
    }

    #[test]
    fn setting_values_keep_spaces_and_may_be_empty() {
        assert_eq!(
            parse("setting\nfont Segoe UI Variable Display"),
            Some(Message::Setting {
                name: "font".to_owned(),
                value: "Segoe UI Variable Display".to_owned(),
            })
        );
        // The empty value is the "back to defaults" case (font).
        assert_eq!(
            parse("setting\nfont "),
            Some(Message::Setting {
                name: "font".to_owned(),
                value: String::new(),
            })
        );
        // No space after the name: not a message.
        assert_eq!(parse("setting\ntheme"), None);
    }

    #[test]
    fn edit_and_link_take_the_whole_rest() {
        assert_eq!(
            parse("edit\nhello\n\nworld"),
            Some(Message::Edit {
                text: "hello\n\nworld".to_owned(),
            })
        );
        // A buffer that itself contains message-shaped lines stays one
        // edit — the `edit\n` prefix wins because it starts the body.
        assert_eq!(
            parse("edit\nsetting\ntheme dark"),
            Some(Message::Edit {
                text: "setting\ntheme dark".to_owned(),
            })
        );
        assert_eq!(
            parse("edit\n"),
            Some(Message::Edit {
                text: String::new(),
            })
        );
        assert_eq!(
            parse("link\nfile:///C:/my%20docs/x.md"),
            Some(Message::Link {
                href: "file:///C:/my%20docs/x.md".to_owned(),
            })
        );
    }

    #[test]
    fn open_export_takes_the_whole_rest() {
        assert_eq!(
            parse("open-export\nC:/my docs/notes.html"),
            Some(Message::OpenExport {
                path: "C:/my docs/notes.html".to_owned(),
            })
        );
        // Taken whole like edit/link: a payload containing
        // message-shaped lines stays one payload.
        assert_eq!(
            parse("open-export\nC:/a\nsave"),
            Some(Message::OpenExport {
                path: "C:/a\nsave".to_owned(),
            })
        );
    }

    #[test]
    fn csv_sort_takes_a_bare_column_index() {
        assert_eq!(parse("csv-sort\n0"), Some(Message::CsvSort { column: 0 }));
        assert_eq!(parse("csv-sort\n12"), Some(Message::CsvSort { column: 12 }));
        assert_eq!(parse("csv-sort\nx"), None);
        assert_eq!(parse("csv-sort\n-1"), None);
        assert_eq!(parse("csv-sort\n"), None);
        assert_eq!(parse("csv-sort"), None);
    }

    #[test]
    fn json_str_quotes_for_javascript() {
        assert_eq!(json_str("hi"), "\"hi\"");
        assert_eq!(json_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_str("\n\r\t"), "\"\\n\\r\\t\"");
        // Line separators trip JS parsers; control characters cannot
        // appear raw in a string literal.
        assert_eq!(json_str("\u{2028}\u{1}"), "\"\\u2028\\u0001\"");
        // Non-ASCII passes through untouched (files are UTF-8 end to end).
        assert_eq!(json_str("héllo — ✓"), "\"héllo — ✓\"");
    }
}
