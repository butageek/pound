//! View layer: the WebView2 shell (Windows).
//!
//! The markdown pane is an embedded WebView2 (via `wry`) showing HTML+CSS —
//! the same class of browser engine VSCode's markdown preview uses, which
//! is what makes its rendering quality possible. Rust keeps the MVP core:
//! the presenter owns the document; the webview only displays it.
//!
//! - Rust -> JS: `webview.evaluate_script` pushes rendered HTML, source
//!   text, title and errors as JSON-escaped strings.
//! - JS -> Rust: the toolbar navigates to `pound://…` URLs, which the
//!   navigation handler intercepts and turns into presenter intents.
//! - Local images are served through the `poundimg://` custom protocol
//!   (like ColaMD's portable file:// image mapping).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::{http, WebView, WebViewBuilder};

use crate::presenter::Presenter;

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

    let builder = WebViewBuilder::new()
        .with_html(SHELL_HTML)
        .with_devtools(cfg!(debug_assertions))
        .with_custom_protocol("poundimg".into(), |_id, request| serve_local_image(request));

    let nav_presenter = shared.clone();
    let nav_dirty = dirty.clone();
    let webview = builder
        .with_navigation_handler(move |url| {
            handle_navigation(&url, &mut nav_presenter.borrow_mut(), &nav_dirty)
        })
        .build(&window)
        .expect("create webview");

    // Paint whatever the initial file (if any) produced.
    push_document(&webview, &shared.borrow());
    apply_title(&window, &shared.borrow());

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
            } => *control_flow = ControlFlow::Exit,

            Event::WindowEvent {
                event: WindowEvent::DroppedFile(path),
                ..
            } => {
                shared.borrow_mut().open_dropped(&[path]);
                dirty.set(true);
            }

            Event::MainEventsCleared => {
                if last_poll.elapsed() >= POLL_INTERVAL {
                    last_poll = Instant::now();
                    shared.borrow_mut().reload_if_changed();
                }
                let revision = shared.borrow().model.revision;
                if dirty.get() || revision != last_pushed {
                    dirty.set(false);
                    last_pushed = revision;
                    let presenter = shared.borrow();
                    push_document(&webview, &presenter);
                    let title = current_title(&presenter);
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

/// Toolbar buttons navigate to `pound://…`; web links open externally.
/// Everything else (the initial document, in-page anchors) is allowed.
fn handle_navigation(url: &str, presenter: &mut Presenter, dirty: &Cell<bool>) -> bool {
    if let Some(command) = url.strip_prefix("pound://") {
        match command.split(['?', '#']).next().unwrap_or("") {
            "open" => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Markdown", &["md", "markdown"])
                    .pick_file()
                {
                    presenter.open_path(&path);
                    dirty.set(true);
                }
            }
            "reload" => {
                presenter.reload();
                dirty.set(true);
            }
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
        Some(doc) => format!("{} — Pound", doc.name()),
        None => "Pound".to_owned(),
    }
}

fn apply_title(window: &tao::window::Window, presenter: &Presenter) {
    window.set_title(&current_title(presenter));
}

/// Push the model into the page. Kept as one JS call so a reload can restore
/// the reading scroll position inside one script evaluation.
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
    let error = model.error.as_deref().unwrap_or("");
    let js = format!(
        "pound.setContent({}, {}, {}, {}); pound.setError({});",
        json_str(html),
        json_str(source),
        json_str(&current_title(presenter)),
        json_str(&path),
        json_str(error),
    );
    if let Err(e) = webview.evaluate_script(&js) {
        eprintln!("pound: failed to update the view: {e}");
    }
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
// The shell document: top bar, rendered pane, source pane. The markdown
// styles use the token set borrowed from ColaMD's themes (GitHub-derived
// palette) with a Segoe UI / Consolas stack for native Windows typography.
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
  #topbar button:hover { background: var(--hover); }
  #topbar .spacer { flex: 1; }
  #path {
    color: var(--muted); font-size: 12px; max-width: 40%;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }
  #topbar label.toggle {
    display: flex; align-items: center; gap: 6px; font-size: 13px;
    cursor: pointer; user-select: none;
  }
  #topbar kbd {
    font: 11px Consolas, monospace; color: var(--muted);
    border: 1px solid var(--border); border-radius: 4px; padding: 0 4px;
  }

  /* ---- error banner ---- */
  #error {
    display: none; align-items: center; gap: 10px;
    position: fixed; top: 50px; left: 50%; transform: translateX(-50%); z-index: 9;
    max-width: 70%; padding: 8px 12px; border-radius: 6px;
    background: var(--error-bg); color: var(--error-fg); font-size: 13px;
    border: 1px solid var(--error-fg);
  }
  #error span { overflow-wrap: anywhere; }

  /* ---- main panes ---- */
  #main { display: flex; height: calc(100vh - 44px); margin-top: 44px; }
  #content-wrap { flex: 1; overflow-y: auto; }
  #source-wrap {
    display: none; width: 42%; min-width: 240px;
    border-left: 1px solid var(--border); overflow: auto; background: var(--bg);
  }
  body.split #source-wrap { display: block; }
  #source {
    margin: 0; padding: 14px 18px 60px;
    font: 12.5px/1.6 Consolas, "Cascadia Mono", monospace;
    white-space: pre; tab-size: 4;
  }

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

  /* ---- markdown content (GitHub/ColaMD token set) ---- */
  #content {
    max-width: 980px; margin: 0 auto; padding: 26px 32px 120px;
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
    <button id="open" type="button">Open&#8230;</button>
    <button id="reload" type="button">Reload</button>
    <span class="spacer"></span>
    <span id="path"></span>
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
    <div id="source-wrap"><pre id="source"></pre></div>
  </div>
<script>
  window.pound = {
    setContent(html, source, title, path) {
      const wrap = document.getElementById('content-wrap');
      const scroll = wrap.scrollTop;
      const content = document.getElementById('content');
      const welcome = document.getElementById('welcome');
      content.innerHTML = html;
      content.style.display = html ? 'block' : 'none';
      welcome.style.display = html ? 'none' : 'flex';
      document.getElementById('source').textContent = source;
      document.getElementById('path').textContent = path;
      if (title) document.title = title;
      wrap.scrollTop = scroll; // keep the reading position on reload
      addCopyButtons();
    },
    setError(msg) {
      const el = document.getElementById('error');
      el.style.display = msg ? 'flex' : 'none';
      el.querySelector('span').textContent = msg || '';
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

  document.getElementById('open').onclick = () => (location.href = 'pound://open');
  document.getElementById('reload').onclick = () => (location.href = 'pound://reload');
  document.getElementById('dismiss').onclick = () => (location.href = 'pound://dismiss-error');

  const toggle = document.getElementById('source-toggle');
  const applySplit = () => document.body.classList.toggle('split', toggle.checked);
  toggle.onchange = applySplit;

  addEventListener('keydown', e => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'u') {
      e.preventDefault();
      toggle.checked = !toggle.checked;
      applySplit();
    }
  });
</script>
</body>
</html>
"#;
