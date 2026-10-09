//! View layer: the WebView2 shell (Windows).
//!
//! The markdown pane is an embedded WebView2 (via `wry`) showing HTML+CSS —
//! the same class of browser engine VSCode's markdown preview uses, which
//! is what makes its rendering quality possible. Rust keeps the MVP core:
//! the presenter owns the document; the webview only displays it.
//!
//! - Rust -> JS: `webview.evaluate_script` pushes rendered HTML, source
//!   text, title and errors as JSON-escaped strings.
//! - JS -> Rust: in-page controls navigate to `pound://…` URLs (the error
//!   banner's dismiss), which the navigation handler intercepts and turns
//!   into presenter intents. The shell additionally posts `ready` via
//!   wry's `window.ipc` once its scripts parsed — the initial content push
//!   races page load, so the view re-pushes on that handshake.
//! - Local images are served through the `poundimg://` custom protocol.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::{http, WebView, WebViewBuilder, WebViewBuilderExtWindows};

use crate::presenter::Presenter;
use crate::update;

/// How often to check the document for on-disk changes.
const POLL_INTERVAL: Duration = Duration::from_millis(800);

pub fn run(file: Option<PathBuf>) {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Pound")
        .with_inner_size(LogicalSize::new(1120.0, 760.0))
        .with_min_inner_size(LogicalSize::new(560.0, 400.0))
        .with_window_icon(window_icon())
        .build(&event_loop)
        .expect("create window");

    let shared = Rc::new(RefCell::new(Presenter::new(file.as_deref())));
    let dirty = Rc::new(Cell::new(false));
    // Set when a push must refresh only the preview (edits/save acks):
    // rewriting the textarea would destroy the caret and scroll position.
    let preview = Rc::new(Cell::new(false));
    // Set when the user confirmed an exit (ipc handlers run inside WebView2
    // message dispatch and cannot touch ControlFlow themselves).
    let exit = Rc::new(Cell::new(false));
    // "Update & restart" with unsaved edits: show the close prompt first
    // and run the installer when the exit actually goes through.
    let update_on_exit = Rc::new(Cell::new(false));
    // Set by ipc when the shell must show the close prompt (the handler
    // cannot reach the webview directly).
    let show_close_prompt = Rc::new(Cell::new(false));

    // Background update check on start: a thread asks the GitHub API and
    // drops the latest tag here (None = no result); the event loop picks
    // it up on its next 800ms wake and shows the toast if it is newer.
    let update_check = Arc::new(Mutex::new(None::<Option<String>>));
    {
        let slot = update_check.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1)); // let startup settle
            let tag = update::latest_release_tag();
            *slot.lock().unwrap() = Some(tag);
        });
    }

    let builder = WebViewBuilder::new()
        .with_html(SHELL_HTML)
        .with_devtools(cfg!(debug_assertions))
        .with_custom_protocol("poundimg".into(), |_id, request| serve_local_image(request))
        // Chromium's default right-click menu (Copy / Print / an empty
        // "More tools" submenu) is browser noise here; the shell shows a
        // minimal Copy-only menu instead.
        .with_default_context_menus(false);

    let nav_presenter = shared.clone();
    let nav_dirty = dirty.clone();
    // Startup handshake: the eager `push_document` below races WebView2's
    // `NavigateToString` commit — when it runs while the initial blank
    // document is still current, `pound` is undefined and the push is lost
    // (wry ignores script exceptions). The shell therefore posts `ready`
    // once its scripts have parsed (see SHELL_HTML) and we re-push then.
    //
    //   "ready"          shell parsed, push the document
    //   "save"           Ctrl+S / Save button
    //   "save-and-exit"  the close prompt's Save: save, then exit
    //   "exit"           the close prompt's Don't save: exit now
    //   "update"         the update toast's Update & restart
    //   "cancel-exit"    the close prompt's Cancel (clears pending update)
    //   "edit\n<text>"   the (debounced) editor buffer changed
    let ipc_presenter = shared.clone();
    let ipc_dirty = dirty.clone();
    let ipc_preview = preview.clone();
    let ipc_exit = exit.clone();
    let ipc_update_on_exit = update_on_exit.clone();
    let ipc_show_close_prompt = show_close_prompt.clone();
    let builder = builder.with_ipc_handler(move |request| {
        let body = request.body();
        // Runs the installer if an update is pending; consumed once.
        let install_if_pending = || {
            if ipc_update_on_exit.replace(false) {
                update::run_installer();
            }
        };
        if body == "ready" {
            ipc_dirty.set(true);
        } else if body == "save" {
            ipc_presenter.borrow_mut().save();
            ipc_preview.set(true);
        } else if body == "save-and-exit" {
            let mut presenter = ipc_presenter.borrow_mut();
            presenter.save();
            let saved = !presenter.model.dirty;
            drop(presenter);
            if saved {
                install_if_pending();
                ipc_exit.set(true);
            } else {
                // The save failed (banner shows why): stay open.
                ipc_preview.set(true);
            }
        } else if body == "exit" {
            install_if_pending();
            ipc_exit.set(true);
        } else if body == "update" {
            if ipc_presenter.borrow().model.dirty {
                // Reuse the close prompt: save or discard, then update.
                ipc_update_on_exit.set(true);
                ipc_show_close_prompt.set(true);
            } else {
                update::run_installer();
                ipc_exit.set(true);
            }
        } else if body == "cancel-exit" {
            ipc_update_on_exit.set(false);
        } else if let Some(text) = body.strip_prefix("edit\n") {
            ipc_presenter.borrow_mut().edit_source(text);
            ipc_preview.set(true);
        }
    });
    let webview = builder
        .with_navigation_handler(move |url| {
            handle_navigation(&url, &mut nav_presenter.borrow_mut(), &nav_dirty)
        })
        .build(&window)
        .expect("create webview");

    // Paint whatever the initial file (if any) produced.
    push_document(&webview, &shared.borrow());
    window.set_title(&current_title(&shared.borrow()));

    let mut last_pushed = shared.borrow().model.revision;
    let mut last_title = current_title(&shared.borrow());
    let mut last_poll = Instant::now();

    event_loop.run(move |event, _, control_flow| {
        // Wake up periodically for the file-change poll.
        *control_flow = ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL);

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // Unsaved edits: ask before quitting (the shell's dialog
                // posts save-and-exit/exit; cancel just closes it).
                if shared.borrow().model.dirty {
                    eval_script(&webview, "pound.setClosePrompt(true);".to_owned());
                } else {
                    *control_flow = ControlFlow::Exit;
                }
            }

            Event::WindowEvent {
                event: WindowEvent::DroppedFile(path),
                ..
            } => {
                shared.borrow_mut().open_dropped(&[path]);
                dirty.set(true);
            }

            Event::MainEventsCleared => {
                if exit.get() {
                    *control_flow = ControlFlow::Exit;
                }
                if show_close_prompt.replace(false) {
                    eval_script(&webview, "pound.setClosePrompt(true);".to_owned());
                }
                // One-shot: show the update toast when a newer release was
                // found by the background check.
                if let Some(tag) = update_check
                    .lock()
                    .unwrap()
                    .take()
                    .flatten()
                    .filter(|tag| update::is_newer(tag, env!("CARGO_PKG_VERSION")))
                {
                    eval_script(&webview, format!("pound.setUpdate({});", json_str(&tag)));
                }
                if last_poll.elapsed() >= POLL_INTERVAL {
                    last_poll = Instant::now();
                    shared.borrow_mut().reload_if_changed();
                }
                let revision = shared.borrow().model.revision;
                let pushed = if preview.get() {
                    // Edit-driven refresh: rendered pane + status only.
                    preview.set(false);
                    last_pushed = revision;
                    push_rendered(&webview, &shared.borrow());
                    true
                } else if dirty.get() || revision != last_pushed {
                    dirty.set(false);
                    last_pushed = revision;
                    push_document(&webview, &shared.borrow());
                    true
                } else {
                    false
                };
                if pushed {
                    let title = current_title(&shared.borrow());
                    if title != last_title {
                        last_title = title;
                        window.set_title(&last_title);
                    }
                }
            }

            _ => {}
        }
    });
}

/// In-page controls navigate to `pound://…`; web links open externally.
/// Everything else (the initial document, in-page anchors) is allowed.
fn handle_navigation(url: &str, presenter: &mut Presenter, dirty: &Cell<bool>) -> bool {
    if let Some(command) = url.strip_prefix("pound://") {
        match command.split(['?', '#']).next().unwrap_or("") {
            "dismiss-error" => {
                presenter.dismiss_error();
                dirty.set(true);
            }
            _ => {}
        }
        return false;
    }

    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        if let Err(e) = open::that(url) {
            presenter.model.error = Some(format!("could not open {url}: {e}"));
            dirty.set(true);
        }
        return false; // never navigate the reader away
    }

    true
}

/// Serve a `poundimg://<percent-encoded absolute path>` request from disk.
fn serve_local_image(
    request: http::Request<Vec<u8>>,
) -> http::Response<std::borrow::Cow<'static, [u8]>> {
    let uri = request.uri().to_string();
    let path = crate::markdown::decode_poundimg_uri(&uri);
    match path.and_then(|path| std::fs::read(&path).ok()) {
        Some(bytes) => http::Response::builder()
            .header("Content-Type", mime_for(&uri))
            .header("Access-Control-Allow-Origin", "*")
            .body(std::borrow::Cow::Owned(bytes))
            .expect("static response headers"),
        None => http::Response::builder()
            .status(404)
            .header("Content-Type", "text/plain")
            .body(std::borrow::Cow::Borrowed(&b"not found"[..]))
            .expect("static response headers"),
    }
}

fn mime_for(uri: &str) -> &'static str {
    let lower = uri.to_ascii_lowercase();
    for (ext, mime) in [
        (".png", "image/png"),
        (".jpg", "image/jpeg"),
        (".jpeg", "image/jpeg"),
        (".gif", "image/gif"),
        (".webp", "image/webp"),
        (".bmp", "image/bmp"),
        (".svg", "image/svg+xml"),
        (".ico", "image/x-icon"),
        (".avif", "image/avif"),
    ] {
        if lower.ends_with(ext) {
            return mime;
        }
    }
    "application/octet-stream"
}

fn current_title(presenter: &Presenter) -> String {
    match &presenter.model.document {
        // `*` marks unsaved edits, the editor convention.
        Some(doc) if presenter.model.dirty => format!("*{} — Pound", doc.name()),
        Some(doc) => format!("{} — Pound", doc.name()),
        None => "Pound".to_owned(),
    }
}

/// Run a shell script. wry ignores JS exceptions (see the ready
/// handshake), but transport failures still surface on stderr.
fn eval_script(webview: &WebView, js: String) {
    if let Err(e) = webview.evaluate_script(&js) {
        eprintln!("pound: failed to update the view: {e}");
    }
}

/// Push the whole model into the page (document opened/reloaded): both
/// panes, status bar, save state, title. The textarea is rewritten, so
/// this must never run for edit-driven updates.
fn push_document(webview: &WebView, presenter: &Presenter) {
    let model = &presenter.model;
    let (html, source, path) = match &model.document {
        Some(doc) => (
            doc.html.as_str(),
            doc.source.as_str(),
            doc.path.display().to_string(),
        ),
        None => ("", "", String::new()),
    };
    eval_script(
        webview,
        format!(
            "pound.setContent({}, {}, {}, {}, {}, {}); pound.setError({});",
            json_str(html),
            json_str(source),
            json_str(&current_title(presenter)),
            json_str(&path),
            json_str(&status_text(presenter)),
            model.dirty,
            json_str(model.error.as_deref().unwrap_or("")),
        ),
    );
}

/// Push only the rendered pane, status bar and save state — the editor
/// buffer (textarea) is left untouched so the caret, selection, undo stack
/// and scroll position survive every keystroke round-trip.
fn push_rendered(webview: &WebView, presenter: &Presenter) {
    let model = &presenter.model;
    let html = model.document.as_ref().map_or("", |doc| doc.html.as_str());
    eval_script(
        webview,
        format!(
            "pound.setRendered({}, {}, {}); pound.setError({});",
            json_str(html),
            json_str(&status_text(presenter)),
            model.dirty,
            json_str(model.error.as_deref().unwrap_or("")),
        ),
    );
}

/// Status-bar text: file type, character and line counts, the edit
/// state, and the app version — e.g. `Markdown · 1,234 characters · 56
/// lines · edited · v0.4.0`.
fn status_text(presenter: &Presenter) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(doc) = &presenter.model.document {
        parts.push(format!(
            "{} · {} characters · {} lines",
            doc.type_label(),
            group_digits(doc.source.chars().count()),
            group_digits(doc.source.lines().count())
        ));
    }
    if presenter.model.dirty {
        parts.push("edited".to_owned());
    }
    if presenter.model.stale {
        parts.push("changed on disk".to_owned());
    }
    parts.push(format!("v{}", env!("CARGO_PKG_VERSION")));
    parts.join(" · ")
}

/// Group digits in threes (1234567 -> 1,234,567).
fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Encode a &str as a double-quoted JavaScript string literal (our own HTML
/// is trusted; this is only about quoting, not XSS — ammonia handles that).
fn json_str(s: &str) -> String {
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

fn window_icon() -> Option<tao::window::Icon> {
    let decoded = image::load_from_memory(include_bytes!("../assets/pound.png")).ok()?;
    let rgba = decoded.to_rgba8();
    let (w, h) = rgba.dimensions();
    tao::window::Icon::from_rgba(rgba.into_raw(), w, h).ok()
}

// ---------------------------------------------------------------------------
// The shell document: top bar, rendered pane, source pane, status bar. The
// markdown styles use a GitHub-style palette with a Segoe UI / Consolas stack
// for native Windows typography.
// ---------------------------------------------------------------------------

const SHELL_HTML: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  :root {
    --bg: #ffffff; --text: #24292f; --muted: #656d76; --border: #d0d7de;
    --link: #0969da; --code-bg: rgba(175,184,193,0.2); --code-block-bg: #f6f8fa;
    --th-bg: #f6f8fa; --hover: rgba(175,184,193,0.25);
    --error-fg: #cf222e; --error-bg: rgba(207,34,46,0.08);
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #0d1117; --text: #e6edf3; --muted: #8b949e; --border: #30363d;
      --link: #58a6ff; --code-bg: rgba(110,118,129,0.4); --code-block-bg: #161b22;
      --th-bg: #161b22; --hover: rgba(110,118,129,0.25);
      --error-fg: #f85149; --error-bg: rgba(248,81,73,0.1);
    }
  }
  * { box-sizing: border-box; }
  html, body { height: 100%; }
  body {
    margin: 0; background: var(--bg); color: var(--text);
    font-family: "Segoe UI", "Segoe UI Variable Text", system-ui, sans-serif;
    overflow: hidden;
  }

  /* ---- top bar ---- */
  #topbar {
    position: fixed; inset: 0 0 auto 0; height: 44px; z-index: 10;
    display: flex; align-items: center; gap: 10px; padding: 0 14px;
    background: var(--bg); border-bottom: 1px solid var(--border);
  }
  #topbar .brand { font-weight: 600; font-size: 14px; margin-right: 2px; }
  #topbar button {
    font: 13px "Segoe UI", system-ui, sans-serif; color: var(--text);
    background: transparent; border: 1px solid var(--border); border-radius: 6px;
    padding: 4px 12px; cursor: pointer;
  }
  #topbar button:hover:not(:disabled) { background: var(--hover); }
  #topbar button:disabled { color: var(--muted); cursor: default; opacity: 0.6; }
  #topbar .spacer { flex: 1; }
  #topbar label.toggle {
    display: flex; align-items: center; gap: 6px; font-size: 13px;
    cursor: pointer; user-select: none;
  }
  #topbar kbd, #ctx-menu kbd {
    font: 11px Consolas, monospace; color: var(--muted);
    border: 1px solid var(--border); border-radius: 4px; padding: 0 4px;
  }

  /* ---- context menu ---- */
  /* The WebView2 default menu is disabled in the builder; this stands in
     with the one action a reader needs: copy the selected text. */
  #ctx-menu {
    display: none; position: fixed; z-index: 20; padding: 4px; min-width: 160px;
    background: var(--bg); border: 1px solid var(--border); border-radius: 8px;
    box-shadow: 0 6px 20px rgba(0,0,0,0.18);
  }
  #ctx-menu.show { display: block; }
  #ctx-copy {
    display: flex; width: 100%; align-items: center; justify-content: space-between;
    font: 13px "Segoe UI", system-ui, sans-serif; color: var(--text);
    background: transparent; border: 0; border-radius: 5px; padding: 6px 10px; cursor: pointer;
  }
  #ctx-copy:hover { background: var(--hover); }

  /* ---- close prompt (unsaved changes) ---- */
  #close-prompt {
    display: none; position: fixed; inset: 0; z-index: 30;
    align-items: center; justify-content: center;
    background: rgba(0,0,0,0.35);
  }
  #close-prompt .dialog {
    min-width: 320px; max-width: 440px; padding: 18px 20px 16px;
    background: var(--bg); border: 1px solid var(--border);
    border-radius: 10px; box-shadow: 0 12px 40px rgba(0,0,0,0.3);
  }
  #close-prompt .dialog-title { font-size: 15px; font-weight: 600; margin-bottom: 6px; }
  #close-prompt .dialog-body {
    font-size: 13px; color: var(--muted); margin-bottom: 16px;
    overflow-wrap: anywhere;
  }
  #close-prompt .dialog-actions { display: flex; justify-content: flex-end; gap: 8px; }

  /* Buttons shared by the close prompt and the update toast. */
  #close-prompt button, #update button {
    font: 13px "Segoe UI", system-ui, sans-serif; color: var(--text);
    background: transparent; border: 1px solid var(--border); border-radius: 6px;
    padding: 5px 14px; cursor: pointer;
  }
  #close-prompt button:hover, #update button:hover { background: var(--hover); }
  #close-prompt button.primary, #update button.primary {
    background: var(--link); border-color: var(--link); color: #fff;
  }
  #close-prompt button.primary:hover, #update button.primary:hover {
    background: var(--link); filter: brightness(1.12);
  }

  /* ---- update toast ---- */
  #update {
    display: none; position: fixed; right: 12px; bottom: 38px; z-index: 25;
    align-items: center; gap: 12px; max-width: 460px; padding: 10px 14px;
    background: var(--bg); border: 1px solid var(--border); border-radius: 8px;
    box-shadow: 0 6px 20px rgba(0,0,0,0.2); font-size: 13px;
  }
  #update a { color: var(--link); text-decoration: none; }
  #update a:hover { text-decoration: underline; }

  /* ---- error banner ---- */
  #error {
    display: none; align-items: center; gap: 10px;
    position: fixed; top: 50px; left: 50%; transform: translateX(-50%); z-index: 9;
    max-width: 70%; padding: 8px 12px; border-radius: 6px;
    background: var(--error-bg); color: var(--error-fg); font-size: 13px;
    border: 1px solid var(--error-fg);
  }
  #error span { overflow-wrap: anywhere; }
  #error button {
    font: 13px "Segoe UI", system-ui, sans-serif; color: var(--error-fg);
    background: transparent; border: 1px solid var(--error-fg); border-radius: 6px;
    padding: 3px 10px; cursor: pointer;
  }

  /* ---- main panes ---- */
  /* 50/50 split: both panes are flex-basis-0 equal-grow siblings, so they
     always divide the available width exactly in half at any window size.
     The 1px divider lives in the container gap (not inside a pane) and
     scrollbar-gutter keeps a scrolling pane from becoming narrower. */
  #main {
    display: flex; height: calc(100vh - 44px - 26px); margin-top: 44px;
    column-gap: 1px; background: var(--border);
  }
  #content-wrap {
    flex: 1 1 0; min-width: 0;
    overflow-y: auto; scrollbar-gutter: stable; background: var(--bg);
  }
  #source-wrap { display: none; flex: 1 1 0; min-width: 0; background: var(--bg); }
  body.split #source-wrap { display: block; }
  /* The source pane is a plain textarea: a real editor with native caret,
     selection and undo. It is never rewritten during editing (pushes go to
     pound.setRendered) so that state survives every keystroke. */
  #source {
    display: block; width: 100%; height: 100%; margin: 0;
    padding: 14px 18px 60px; border: 0; outline: none; resize: none;
    background: transparent; color: var(--text);
    font: 12.5px/1.6 Consolas, "Cascadia Mono", monospace;
    white-space: pre; tab-size: 4;
    overflow: auto; scrollbar-gutter: stable;
  }

  /* ---- status bar ---- */
  #statusbar {
    position: fixed; inset: auto 0 0 0; height: 26px; z-index: 10;
    display: flex; align-items: center; gap: 14px; padding: 0 12px;
    background: var(--bg); border-top: 1px solid var(--border);
    color: var(--muted); font-size: 12px; user-select: none;
  }
  #status-path {
    flex: 1 1 auto; min-width: 0;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }
  #status-info { flex: 0 0 auto; user-select: none; }

  /* ---- welcome ---- */
  #welcome {
    display: flex; flex-direction: column; align-items: center;
    gap: 8px; padding-top: 24vh; color: var(--muted); text-align: center;
  }
  #welcome h1 { color: var(--text); font-size: 28px; margin: 0 0 4px; }
  #welcome code {
    background: var(--code-bg); border-radius: 5px; padding: 1px 5px;
    font-family: Consolas, monospace; font-size: 12.5px;
  }

  /* ---- markdown content (GitHub-style palette) ---- */
  #content {
    /* Full pane width like VSCode's preview (no centered reading column). */
    padding: 26px 40px 120px;
    font-size: 15.5px; line-height: 1.65;
  }
  #content h1, #content h2, #content h3, #content h4, #content h5, #content h6 {
    font-weight: 600; line-height: 1.25; margin: 26px 0 14px; scroll-margin-top: 60px;
  }
  #content h1 { font-size: 1.9em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
  #content h2 { font-size: 1.45em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
  #content h3 { font-size: 1.22em; } #content h4 { font-size: 1.05em; }
  #content h5 { font-size: 0.95em; } #content h6 { font-size: 0.87em; color: var(--muted); }
  #content p { margin: 0 0 16px; }
  #content a { color: var(--link); text-decoration: none; }
  #content a:hover { text-decoration: underline; }
  #content code {
    background: var(--code-bg); border-radius: 6px; padding: .2em .4em;
    font-family: Consolas, "Cascadia Mono", monospace; font-size: 85%;
  }
  #content pre {
    position: relative; background: var(--code-block-bg); border-radius: 6px;
    padding: 14px 16px; margin: 0 0 16px; overflow: auto; line-height: 1.45;
  }
  #content pre code { background: none; padding: 0; font-size: 13px; }
  #content pre .copy-btn {
    position: absolute; top: 6px; right: 6px; opacity: 0;
    font: 11px "Segoe UI", sans-serif; color: var(--muted);
    background: var(--bg); border: 1px solid var(--border); border-radius: 5px;
    padding: 2px 8px; cursor: pointer;
  }
  #content pre:hover .copy-btn { opacity: 1; }
  #content blockquote {
    margin: 0 0 16px; padding: 0 1em; color: var(--muted);
    border-left: .25em solid var(--border);
  }
  #content table {
    border-collapse: collapse; display: block; max-width: 100%;
    overflow: auto; margin: 0 0 16px;
  }
  #content th, #content td { border: 1px solid var(--border); padding: 6px 13px; }
  #content th { background: var(--th-bg); font-weight: 600; }
  #content img { max-width: 100%; }
  #content hr { border: 0; border-top: 1px solid var(--border); margin: 24px 0; }
  #content ul, #content ol { padding-left: 2em; margin: 0 0 16px; }
  #content li { margin: .25em 0; }
  #content li:has(> input[type="checkbox"]) { list-style: none; margin-left: -1.2em; }
  #content input[type="checkbox"] { margin: 0 .6em 0 0; vertical-align: middle; }
  #content kbd {
    font: 85% Consolas, monospace; border: 1px solid var(--border);
    border-radius: 4px; padding: 1px 5px; background: var(--code-block-bg);
  }
  #content del { color: var(--muted); }
</style>
</head>
<body>
  <div id="topbar">
    <span class="brand">Pound</span>
    <span class="spacer"></span>
    <button id="save" type="button" disabled>Save <kbd>Ctrl+S</kbd></button>
    <label class="toggle">
      <input type="checkbox" id="source-toggle"> Source <kbd>Ctrl+U</kbd>
    </label>
  </div>
  <div id="error"><span></span><button id="dismiss" type="button">Dismiss</button></div>
  <div id="main">
    <div id="content-wrap">
      <div id="welcome">
        <h1>Pound</h1>
        <div>A tiny markdown reader.</div>
        <div>Drop a <code>.md</code> file anywhere in this window,</div>
        <div>or run <code>pound path/to/file.md</code> from a terminal.</div>
      </div>
      <article id="content"></article>
    </div>
    <div id="source-wrap"><textarea id="source" wrap="off" spellcheck="false"></textarea></div>
  </div>
  <div id="statusbar">
    <span id="status-path"></span>
    <span id="status-info"></span>
  </div>
  <div id="ctx-menu"><button id="ctx-copy" type="button">Copy <kbd>Ctrl+C</kbd></button></div>
  <div id="update">
    <span>Pound <b id="update-version"></b> is available.</span>
    <a id="update-notes" href="">What's new</a>
    <button id="update-now" class="primary" type="button">Update &amp; restart</button>
    <button id="update-dismiss" type="button">Later</button>
  </div>
  <div id="close-prompt">
    <div class="dialog">
      <div class="dialog-title">Save changes?</div>
      <div class="dialog-body" id="close-prompt-body"></div>
      <div class="dialog-actions">
        <button id="close-cancel" type="button">Cancel</button>
        <button id="close-discard" type="button">Don't save</button>
        <button id="close-save" class="primary" type="button">Save</button>
      </div>
    </div>
  </div>
<script>
  window.pound = {
    setContent(html, source, title, path, status, edited) {
      const wrap = document.getElementById('content-wrap');
      const scroll = wrap.scrollTop;
      const content = document.getElementById('content');
      const welcome = document.getElementById('welcome');
      content.innerHTML = html;
      content.style.display = html ? 'block' : 'none';
      welcome.style.display = html ? 'none' : 'flex';
      sourceEl.value = source;
      // Park the caret at the top: a value assignment leaves it at the
      // end, so the first focus would auto-scroll the pane to the bottom.
      sourceEl.setSelectionRange(0, 0);
      const statusPath = document.getElementById('status-path');
      statusPath.textContent = path || 'Ready';
      statusPath.title = path; // hover tooltip: the untruncated path
      document.getElementById('status-info').textContent = status;
      setSaveEnabled(edited);
      if (title) document.title = title;
      wrap.scrollTop = scroll; // keep the reading position on reload
      addCopyButtons();
    },
    // Edit-driven refresh (see push_rendered): update the preview, status
    // bar and save state WITHOUT touching the editor buffer.
    setRendered(html, status, edited) {
      const wrap = document.getElementById('content-wrap');
      const scroll = wrap.scrollTop;
      document.getElementById('content').innerHTML = html;
      wrap.scrollTop = scroll;
      document.getElementById('status-info').textContent = status;
      setSaveEnabled(edited);
      addCopyButtons();
    },
    setError(msg) {
      const el = document.getElementById('error');
      el.style.display = msg ? 'flex' : 'none';
      el.querySelector('span').textContent = msg || '';
    },
    // Shown by the host when the window is closed with unsaved edits.
    // The buttons answer over ipc: Save -> "save-and-exit" (the host only
    // exits if the save succeeded), Don't save -> "exit", Cancel ->
    // "cancel-exit" (clears a pending update; the host keeps running).
    setClosePrompt(show) {
      const prompt = document.getElementById('close-prompt');
      if (!show) { prompt.style.display = 'none'; return; }
      const path = document.getElementById('status-path').textContent;
      const name = path.split(/[\\/]/).filter(Boolean).pop() || 'This document';
      document.getElementById('close-prompt-body').textContent =
        name + ' has unsaved changes. Save before closing?';
      prompt.style.display = 'flex';
      document.getElementById('close-save').focus();
    },
    // Shown once when the start-up check found a newer GitHub release.
    // "Update & restart" runs the official installer (the app exits, the
    // installer replaces the exe and relaunches it); the notes link opens
    // externally like every other web link.
    setUpdate(version) {
      document.getElementById('update-version').textContent = version;
      document.getElementById('update-notes').href =
        'https://github.com/butageek/pound/releases/tag/' + encodeURIComponent(version);
      document.getElementById('update').style.display = 'flex';
    },
  };

  function addCopyButtons() {
    document.querySelectorAll('#content pre').forEach(pre => {
      if (pre.querySelector('.copy-btn')) return;
      const btn = document.createElement('button');
      btn.type = 'button'; btn.className = 'copy-btn'; btn.textContent = 'Copy';
      btn.onclick = () => {
        copyText(pre.innerText).then(() => {
          btn.textContent = 'Copied';
          setTimeout(() => (btn.textContent = 'Copy'), 1200);
        });
      };
      pre.appendChild(btn);
    });
  }

  function copyText(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text).catch(() => legacyCopy(text));
    }
    return Promise.resolve(legacyCopy(text));
  }

  function legacyCopy(text) {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed'; ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); } finally { ta.remove(); }
  }

  document.getElementById('dismiss').onclick = () => (location.href = 'pound://dismiss-error');

  const toggle = document.getElementById('source-toggle');
  const applySplit = () => {
    document.body.classList.toggle('split', toggle.checked);
    // Opening the source pane aligns it with what's on screen (VSCode-style).
    if (toggle.checked) syncSourceFromContent();
  };
  toggle.onchange = applySplit;

  addEventListener('keydown', e => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'u') {
      e.preventDefault();
      toggle.checked = !toggle.checked;
      applySplit();
    }
  });

  // ---- scroll sync (VSCode-style) ----------------------------------
  // The renderer wraps each top-level block in a div carrying its source
  // line range (data-line-start/-end). Scrolling either pane scrolls the
  // other to the matching position, interpolated proportionally within a
  // block. Programmatic sets stamp a short per-pane suppression window so
  // the echo of our own scrolling never re-triggers the sync.
  const contentWrap = document.getElementById('content-wrap');
  const sourceEl = document.getElementById('source');
  const suppressEchoUntil = { source: 0, content: 0 };
  const setScrollTop = (which, el, y) => {
    suppressEchoUntil[which] = Date.now() + 250;
    el.scrollTop = Math.max(0, y);
  };
  const blocksWithLines = () =>
    [...document.querySelectorAll('#content > [data-line-start]')].map(el => {
      const s = +el.dataset.lineStart;
      return { el, s, e: Math.max(+el.dataset.lineEnd, s + 1) };
    });
  // The source pane is fixed-metrics text (12.5px/1.6 + padding): a line
  // height and top padding turn a scroll offset into a (fractional) line
  // number and back. The textarea is its own scroller.
  const sourceMetrics = () => {
    const cs = getComputedStyle(sourceEl);
    return { lh: parseFloat(cs.lineHeight) || 20, pad: parseFloat(cs.paddingTop) || 0 };
  };
  const blockTop = (el, scrollTop) =>
    el.getBoundingClientRect().top - contentWrap.getBoundingClientRect().top + scrollTop;

  function syncContentFromSource() {
    if (!document.body.classList.contains('split')) return;
    const blocks = blocksWithLines();
    if (!blocks.length) return;
    const { lh, pad } = sourceMetrics();
    const line = 1 + (sourceEl.scrollTop - pad) / lh;
    let t = blocks[0];
    for (const b of blocks) { if (b.s <= line) t = b; else break; }
    const frac = Math.min(Math.max((line - t.s) / (t.e - t.s), 0), 1);
    setScrollTop('content', contentWrap, blockTop(t.el, contentWrap.scrollTop) + frac * t.el.offsetHeight);
  }

  function syncSourceFromContent() {
    if (!document.body.classList.contains('split')) return;
    const blocks = blocksWithLines();
    if (!blocks.length) return;
    const viewY = contentWrap.scrollTop;
    let t = blocks[0];
    for (const b of blocks) { if (blockTop(b.el, viewY) <= viewY) t = b; else break; }
    const top = blockTop(t.el, viewY);
    const frac = Math.min(Math.max((viewY - top) / Math.max(t.el.offsetHeight, 1), 0), 1);
    const line = t.s + frac * (t.e - t.s);
    const { lh, pad } = sourceMetrics();
    setScrollTop('source', sourceEl, pad + (line - 1) * lh);
  }

  sourceEl.addEventListener('scroll', () => {
    if (Date.now() >= suppressEchoUntil.source) syncContentFromSource();
  }, { passive: true });
  contentWrap.addEventListener('scroll', () => {
    if (Date.now() >= suppressEchoUntil.content) syncSourceFromContent();
  }, { passive: true });

  // ---- editor: live preview + save -----------------------------------
  const saveBtn = document.getElementById('save');
  const setSaveEnabled = on => (saveBtn.disabled = !on);
  const hostSend = msg => { if (window.ipc) window.ipc.postMessage(msg); };
  saveBtn.onclick = () => hostSend('save');

  // Typing: debounce, then hand the buffer to the host for re-rendering
  // (Rust owns rendering + sanitization; the result comes back through
  // pound.setRendered, which never touches this textarea).
  let editTimer = 0;
  sourceEl.addEventListener('input', () => {
    clearTimeout(editTimer);
    editTimer = setTimeout(() => hostSend('edit\n' + sourceEl.value), 250);
  });

  addEventListener('keydown', e => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
      e.preventDefault();
      if (!saveBtn.disabled) hostSend('save');
    }
  });

  // Tab indents (4 spaces, matching tab-size) instead of leaving the pane;
  // execCommand keeps the insertion on the native undo stack.
  sourceEl.addEventListener('keydown', e => {
    if (e.key === 'Tab' && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      document.execCommand('insertText', false, '    ');
    }
  });

  // ---- context menu --------------------------------------------------
  // Minimal replacement for the disabled WebView2 default menu: a lone
  // Copy action, shown only when text is selected.
  const ctxMenu = document.getElementById('ctx-menu');
  const hideCtxMenu = () => ctxMenu.classList.remove('show');
  // Selection text for Copy; the textarea too — form-control selections
  // are not reliably reflected in window.getSelection().
  const selectedText = () => {
    const s = String(getSelection());
    if (s) return s;
    const el = document.activeElement;
    if (el && el.value && el.selectionStart != null && el.selectionEnd != null) {
      return el.value.slice(el.selectionStart, el.selectionEnd);
    }
    return '';
  };
  document.addEventListener('contextmenu', e => {
    e.preventDefault();
    if (!selectedText().trim()) { hideCtxMenu(); return; }
    ctxMenu.classList.add('show');
    // Open at the cursor, nudged back inside the window if it would overflow.
    ctxMenu.style.left = Math.min(e.clientX, innerWidth - ctxMenu.offsetWidth - 4) + 'px';
    ctxMenu.style.top = Math.min(e.clientY, innerHeight - ctxMenu.offsetHeight - 4) + 'px';
  });
  document.getElementById('ctx-copy').onclick = () => {
    copyText(selectedText());
    hideCtxMenu();
  };
  addEventListener('click', hideCtxMenu);
  addEventListener('blur', hideCtxMenu);
  addEventListener('scroll', hideCtxMenu, true); // scrolling any pane would leave the menu stale
  addEventListener('keydown', e => { if (e.key === 'Escape') hideCtxMenu(); });

  // ---- unsaved-changes prompt ----------------------------------------
  const closePrompt = document.getElementById('close-prompt');
  const hideClosePrompt = () => (closePrompt.style.display = 'none');
  const answerClosePrompt = msg => {
    hideClosePrompt();
    hostSend(msg);
  };
  document.getElementById('close-save').onclick = () => answerClosePrompt('save-and-exit');
  document.getElementById('close-discard').onclick = () => answerClosePrompt('exit');
  document.getElementById('close-cancel').onclick = () => answerClosePrompt('cancel-exit');
  addEventListener('keydown', e => {
    if (closePrompt.style.display !== 'flex') return;
    if (e.key === 'Escape') { e.preventDefault(); answerClosePrompt('cancel-exit'); }
    if (e.key === 'Enter') { e.preventDefault(); answerClosePrompt('save-and-exit'); }
  });

  // ---- update toast ---------------------------------------------------
  const updateToast = document.getElementById('update');
  document.getElementById('update-now').onclick = () => {
    updateToast.style.display = 'none';
    hostSend('update');
  };
  document.getElementById('update-dismiss').onclick = () =>
    (updateToast.style.display = 'none');

  // Tell the host this shell has parsed and pound.setContent is callable:
  // the initial content push races the page load, so the host re-sends on
  // 'ready' (window.ipc exists only inside the app — in a plain browser
  // this is a no-op, keeping the preview recipe usable).
  if (window.ipc) window.ipc.postMessage('ready');
</script>
</body>
</html>
"#;
