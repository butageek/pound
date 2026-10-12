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

use tao::dpi::{LogicalSize, PhysicalSize, Size};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::{http, DragDropEvent, WebView, WebViewBuilder, WebViewBuilderExtWindows};

use crate::fonts;
use crate::presenter::{link_action, LinkAction, Presenter};
use crate::register;
use crate::theme::{DarkPalette, LightPalette, Settings, Theme};
use crate::update;
/// The shell with the persisted settings and the installed font list
/// injected (`window.poundSettings` / `window.poundFonts`): the head
/// script resolves the theme and font before first paint, so the app
/// never flashes the wrong ones.
fn shell_html(settings: &Settings, fonts: &[String]) -> String {
    const PLACEHOLDER: &str =
        "window.poundSettings = { mode: 'auto', light: 'solarized', dark: 'one-dark', font: '', size: 15.5 };";
    const FONTS_PLACEHOLDER: &str = "window.poundFonts = [];";
    let font = match &settings.font {
        Some(name) => json_str(name),
        None => "''".to_owned(),
    };
    let settings_injected = format!(
        "window.poundSettings = {{ mode: '{}', light: '{}', dark: '{}', font: {}, size: {} }};",
        settings.theme.as_str(),
        settings.light.as_str(),
        settings.dark.as_str(),
        font,
        settings.font_size,
    );
    let quoted: Vec<String> = fonts.iter().map(|n| json_str(n)).collect();
    let fonts_injected = format!("window.poundFonts = [{}];", quoted.join(", "));
    SHELL_HTML
        .replace(PLACEHOLDER, &settings_injected)
        .replace(FONTS_PLACEHOLDER, &fonts_injected)
}

/// How often to check the document for on-disk changes.
const POLL_INTERVAL: Duration = Duration::from_millis(800);

/// The pre-paint background color for the window and the webview: the
/// resolved palette's `--bg` as opaque RGBA. Computed from whatever
/// settings are in effect — the persisted ones at startup, fresh ones
/// after a settings-page change (see the `theme_changed` flag).
fn theme_background_of(settings: &Settings) -> (u8, u8, u8, u8) {
    let bg = settings.theme.resolved_background(
        settings.light,
        settings.dark,
        register::system_prefers_dark(),
    );
    (bg[0], bg[1], bg[2], 255)
}

pub fn run(file: Option<PathBuf>) {
    let event_loop = EventLoop::new();

    // Installed fonts: enumerated once, used to build the settings page
    // list, to validate the persisted name (a font that was uninstalled
    // falls back to the default) and to validate ipc saves below.
    let fonts = fonts::families();
    let mut settings = register::load_settings();
    // `take().filter` keeps the persisted name only while it is installed.
    settings.font = settings
        .font
        .take()
        .filter(|name| fonts.iter().any(|f| f == name));

    // Pre-paint background: the window (Win32 erase brush) and the
    // WebView2 surface both start in the theme's `--bg`, so dark themes
    // never flash white at startup or exit. The shell corrects Auto
    // live via matchMedia after paint.
    let background = theme_background_of(&settings);

    // Restore the last window size (saved on resize, see the Resized
    // handler in the event loop); physical pixels, because that is what
    // resize events report. Default 1120×760 logical for a first launch.
    let size = match register::window_size() {
        Some((width, height)) => Size::Physical(PhysicalSize::new(width, height)),
        None => Size::Logical(LogicalSize::new(1120.0, 760.0)),
    };
    let window = WindowBuilder::new()
        .with_title("Pound")
        .with_inner_size(size)
        .with_min_inner_size(LogicalSize::new(560.0, 400.0))
        .with_background_color(background)
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
    // Set by ipc when the toast must switch to progress mode (same reason).
    let show_update_progress = Rc::new(Cell::new(false));
    // Set by ipc when a theme/palette change was persisted: the window
    // and webview pre-paint backgrounds must follow, so closing never
    // flashes the startup theme's color (applied in MainEventsCleared —
    // the ipc handler cannot touch the window).
    let theme_changed = Rc::new(Cell::new(false));
    // The running installer's streamed progress (see update::InstallerRun).
    // Shared with the ipc handler, which spawns it.
    let installer: Arc<update::InstallerSlot> = Arc::new(Mutex::new(None));
    // Clipboard text awaiting delivery to the shell (the Paste action:
    // the ipc handler cannot evaluate scripts, so the loop sends it).
    let clipboard_reply: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

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
        .with_html(shell_html(&settings, &fonts))
        // Same pre-paint color as the window: the WebView2 surface must
        // not show its default white before the shell paints either.
        .with_background_color(background)
        .with_devtools(cfg!(debug_assertions))
        .with_custom_protocol("poundimg".into(), |_id, request| serve_local_image(request))
        // Chromium's default right-click menu (Copy / Print / an empty
        // "More tools" submenu) is browser noise here; the shell shows a
        // minimal Copy-only menu instead.
        .with_default_context_menus(false);

    let nav_presenter = shared.clone();
    let nav_dirty = dirty.clone();
    // The navigation handler's first call is the shell document's own
    // load — allowed whatever URL the platform reports (see link_action).
    let nav_initial = Rc::new(Cell::new(true));
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
    //   "setting\n<n> <v>" a settings-page change: validated, persisted
    //                     (the shell already applied it locally);
    //                     theme/palette ones also repaint the window
    //                     background (via the theme_changed flag)
    //   "clipboard-read" the editor's Paste: host reads the Win32
    //                     clipboard, replies via pound.setClipboardText
    //   "link\n<href>"    a link click in the rendered pane: web links
    //                     open in the browser, file targets in a new
    //                     Pound window (which explains a miss itself)
    //   "edit\n<text>"   the (debounced) editor buffer changed
    let ipc_presenter = shared.clone();
    let ipc_dirty = dirty.clone();
    let ipc_preview = preview.clone();
    let ipc_exit = exit.clone();
    let ipc_update_on_exit = update_on_exit.clone();
    let ipc_show_close_prompt = show_close_prompt.clone();
    let ipc_update_ui = show_update_progress.clone();
    let ipc_installer = Arc::clone(&installer);
    let ipc_clipboard_reply = Arc::clone(&clipboard_reply);
    let ipc_theme_changed = theme_changed.clone();
    // Font list for validating settings-page saves (see fonts::families).
    let ipc_fonts = fonts.clone();
    let builder = builder.with_ipc_handler(move |request| {
        let body = request.body();
        // Spawn the installer and switch the toast to progress mode. The
        // app stays alive showing progress until the installer's graceful
        // close closes it at the replace step; a failed spawn returns
        // false so the caller can fall back (plain exit / keep open).
        let begin_update = |open_path: Option<&std::path::Path>| -> bool {
            match update::spawn_installer(open_path) {
                Ok(child) => {
                    *ipc_installer.lock().unwrap() = Some(update::InstallerRun::default());
                    let slot = Arc::clone(&ipc_installer);
                    std::thread::spawn(move || update::read_installer_output(child, &slot));
                    ipc_update_ui.set(true);
                    true
                }
                Err(e) => {
                    ipc_presenter.borrow_mut().model.error =
                        Some(format!("could not start the updater: {e}"));
                    ipc_preview.set(true);
                    false
                }
            }
        };
        if body == "ready" {
            ipc_dirty.set(true);
        } else if body == "save" {
            ipc_presenter.borrow_mut().save();
            ipc_preview.set(true);
        } else if body == "save-and-exit" {
            let open_path = ipc_presenter
                .borrow()
                .model
                .document
                .as_ref()
                .map(|doc| doc.path.clone());
            let mut presenter = ipc_presenter.borrow_mut();
            presenter.save();
            let saved = !presenter.model.dirty;
            drop(presenter);
            if saved {
                // Progress mode: the installer closes us at the replace
                // step; a failed spawn falls back to a plain close.
                if !begin_update(open_path.as_deref()) {
                    ipc_exit.set(true);
                }
            } else {
                // The save failed (banner shows why): stay open.
                ipc_preview.set(true);
            }
        } else if body == "exit" {
            let open_path = ipc_presenter
                .borrow()
                .model
                .document
                .as_ref()
                .map(|doc| doc.path.clone());
            if !begin_update(open_path.as_deref()) {
                ipc_exit.set(true);
            }
        } else if body == "update" {
            if ipc_presenter.borrow().model.dirty {
                // Reuse the close prompt: save or discard, then update.
                ipc_update_on_exit.set(true);
                ipc_show_close_prompt.set(true);
            } else {
                let open_path = ipc_presenter
                    .borrow()
                    .model
                    .document
                    .as_ref()
                    .map(|doc| doc.path.clone());
                begin_update(open_path.as_deref());
            }
        } else if body == "cancel-exit" {
            ipc_update_on_exit.set(false);
        } else if let Some(setting) = body.strip_prefix("setting\n") {
            if let Some((name, value)) = setting.split_once(' ') {
                // Validate before persisting; map to the registry value name.
                // The font accepts "" (back to the default stacks) or any
                // installed family; names are free-form and may contain
                // spaces, which split_once keeps in the value.
                let (valid, reg_name) = match name {
                    "theme" => (Theme::parse(value).is_some(), "Theme"),
                    "light" => (LightPalette::parse(value).is_some(), "LightPalette"),
                    "dark" => (DarkPalette::parse(value).is_some(), "DarkPalette"),
                    "font" => (
                        value.is_empty() || ipc_fonts.iter().any(|f| f == value),
                        "Font",
                    ),
                    "size" => (crate::theme::parse_font_size(value).is_some(), "FontSize"),
                    _ => (false, ""),
                };
                if valid {
                    register::save_setting(reg_name, value);
                    // Theme/palette changes must also repaint the
                    // pre-paint backgrounds (see the theme_changed flag).
                    if matches!(name, "theme" | "light" | "dark") {
                        ipc_theme_changed.set(true);
                    }
                }
            }
        } else if body == "clipboard-read" {
            // A non-text clipboard simply yields no reply: Paste no-ops.
            if let Some(text) = crate::clipboard::read_text() {
                *ipc_clipboard_reply.lock().unwrap() = Some(text);
            }
        } else if let Some(href) = body.strip_prefix("link\n") {
            // The shell intercepts link clicks (the browser resolves
            // hrefs against about:blank, mangling them before any
            // navigation handler could see them).
            match link_action(href, false) {
                LinkAction::OpenExternally => {
                    if let Err(e) = open::that(href) {
                        let message = format!("could not open {href}: {e}");
                        ipc_presenter.borrow_mut().model.error = Some(message);
                        ipc_dirty.set(true);
                    }
                }
                LinkAction::Refuse => {
                    // A local-file target: open it in a new Pound window,
                    // VSCode-style — a missing file shows its own error
                    // banner there instead of trapping this window.
                    let target = ipc_presenter.borrow().link_target(href);
                    if let Some(path) = target {
                        // A detached child: dropping the handle does not
                        // kill the process, so the window outlives us.
                        let spawned = std::env::current_exe()
                            .and_then(|exe| std::process::Command::new(exe).arg(&path).spawn());
                        if let Err(e) = spawned {
                            let message =
                                format!("could not open a new window for {}: {e}", path.display());
                            ipc_presenter.borrow_mut().model.error = Some(message);
                            ipc_dirty.set(true);
                        }
                    }
                }
                LinkAction::Stay | LinkAction::Internal => {
                    // Anchors scroll in the shell; pound:// controls still
                    // travel through the navigation handler.
                }
            }
        } else if let Some(text) = body.strip_prefix("edit\n") {
            ipc_presenter.borrow_mut().edit_source(text);
            ipc_preview.set(true);
        }
    });
    // File drops: WebView2 forwards them to the host only when a
    // drag-drop handler is registered (wry sets AllowExternalDrop(false)
    // and installs its own controller) — without one, the page silently
    // swallows them and tao's DroppedFile never fires because the webview
    // covers the whole client area.
    let drop_presenter = shared.clone();
    let drop_dirty = dirty.clone();
    let builder = builder.with_drag_drop_handler(move |event| {
        if let DragDropEvent::Drop { paths, .. } = event {
            drop_presenter.borrow_mut().open_dropped(&paths);
            drop_dirty.set(true);
        }
        true
    });
    let webview = builder
        .with_navigation_handler(move |url| {
            let initial_load = nav_initial.replace(false);
            handle_navigation(
                &url,
                initial_load,
                &mut nav_presenter.borrow_mut(),
                &nav_dirty,
            )
        })
        .build(&window)
        .expect("create webview");

    // Paint whatever the initial file (if any) produced.
    push_document(&webview, &shared.borrow());
    window.set_title(&current_title(&shared.borrow()));

    let mut last_pushed = shared.borrow().model.revision;
    let mut last_title = current_title(&shared.borrow());
    let mut last_poll = Instant::now();
    // Update-progress UI state: the toast switched to progress mode, and
    // how many installer lines have been mirrored into it so far.
    let mut update_ui_active = false;
    let mut update_lines_shown = 0usize;

    event_loop.run(move |event, _, control_flow| {
        // Wake up periodically for the file-change poll.
        *control_flow = ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL);

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // Unsaved edits: ask before quitting (the shell's dialog
                // posts save-and-exit/exit; cancel just closes it). While
                // an update is running the buffer is clean by definition —
                // the installer will close us at the replace step.
                let updating = installer.lock().unwrap().is_some();
                if shared.borrow().model.dirty && !updating {
                    eval_script(&webview, "pound.setClosePrompt(true);".to_owned());
                } else {
                    *control_flow = ControlFlow::Exit;
                }
            }

            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                // Remember the size so the next launch opens like this one.
                // Skip maximized windows: the restore size is the one
                // worth keeping (maximizing is not remembered itself).
                if !window.is_maximized() {
                    register::save_setting("WindowWidth", &size.width.to_string());
                    register::save_setting("WindowHeight", &size.height.to_string());
                }
            }

            Event::MainEventsCleared => {
                if exit.get() {
                    *control_flow = ControlFlow::Exit;
                }
                if show_close_prompt.replace(false) {
                    eval_script(&webview, "pound.setClosePrompt(true);".to_owned());
                }
                // A theme/palette change was persisted: follow it with the
                // pre-paint colors so closing the app never flashes the
                // startup theme. Settings are re-read fresh (the ipc
                // handler has already persisted them).
                if theme_changed.replace(false) {
                    let background = theme_background_of(&register::load_settings());
                    window.set_background_color(Some(background));
                    // Best-effort: the painted shell covers this surface;
                    // the window brush above is the load-bearing one.
                    let _ = webview.set_background_color(background);
                }
                // Deliver clipboard text requested by the editor's Paste.
                if let Some(text) = clipboard_reply.lock().unwrap().take() {
                    eval_script(
                        &webview,
                        format!("pound.setClipboardText({});", json_str(&text)),
                    );
                }
                // Update progress: switch the toast to progress mode once,
                // then mirror the installer's latest line until it exits.
                if show_update_progress.replace(false) {
                    update_ui_active = true;
                    update_lines_shown = 0;
                    eval_script(&webview, "pound.setUpdating();".to_owned());
                }
                if update_ui_active {
                    let guard = installer.lock().unwrap();
                    if let Some(run) = guard.as_ref() {
                        if run.lines.len() > update_lines_shown {
                            update_lines_shown = run.lines.len();
                            let line = run.lines.last().cloned().unwrap_or_default();
                            eval_script(
                                &webview,
                                format!("pound.setUpdatingProgress({});", json_str(&line)),
                            );
                        }
                        if run.finished {
                            // The installer completed but did not close us
                            // (e.g. a portable exe elsewhere): exit so the
                            // freshly installed/relaunched Pound takes over.
                            *control_flow = ControlFlow::Exit;
                        }
                    }
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

/// Decide one navigation: `pound://` controls run their intent, web
/// links open externally, in-page anchors stay — everything else is
/// refused with a message. The rules, and why the reader never
/// navigates away from the shell document, live in [`link_action`]
/// (`initial_load` allows the shell's own first load).
fn handle_navigation(
    url: &str,
    initial_load: bool,
    presenter: &mut Presenter,
    dirty: &Cell<bool>,
) -> bool {
    match link_action(url, initial_load) {
        LinkAction::Internal => {
            if let Some(command) = url.strip_prefix("pound://") {
                match command.split(['?', '#']).next().unwrap_or("") {
                    "dismiss-error" => {
                        presenter.dismiss_error();
                        dirty.set(true);
                    }
                    _ => {}
                }
            }
            false
        }
        LinkAction::OpenExternally => {
            if let Err(e) = open::that(url) {
                presenter.model.error = Some(format!("could not open {url}: {e}"));
                dirty.set(true);
            }
            false
        }
        LinkAction::Refuse => {
            presenter.model.error = Some(format!("link cannot be opened in the reader: {url}"));
            dirty.set(true);
            false
        }
        LinkAction::Stay => true,
    }
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
    let (html, source, path, plain) = match &model.document {
        Some(doc) => (
            doc.html.as_str(),
            doc.source.as_str(),
            doc.path.display().to_string(),
            doc.is_plain(),
        ),
        None => ("", "", String::new(), false),
    };
    eval_script(
        webview,
        format!(
            "pound.setContent({}, {}, {}, {}, {}, {}, {}); pound.setError({});",
            json_str(html),
            json_str(source),
            json_str(&current_title(presenter)),
            json_str(&path),
            json_str(&status_text(presenter)),
            model.dirty,
            plain,
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
// markdown styles use the Solarized Light / One Dark palettes with a
// Segoe UI / Consolas stack for native Windows typography.
// ---------------------------------------------------------------------------

const SHELL_HTML: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  /* The theme is resolved by the head script (the persisted settings are
     injected as window.poundSettings; the mode may be Auto = follow the
     system) and applied as <html data-theme="<palette>">. color-scheme
     themes scrollbars and form controls to match. Palettes — the editor
     world's eye-comfort standards: Solarized Light (cream), Catppuccin
     Latte (soft gray), One Dark (Atom/Zed's slate), Catppuccin Mocha
     (muted indigo night). */
  :root {
    color-scheme: light;
    --bg: #fdf6e3; --text: #586e75; --muted: #93a1a1; --border: #eee8d5;
    --link: #268bd2; --code-bg: rgba(147,161,161,0.18); --code-block-bg: #f3ecda;
    --th-bg: #eee8d5; --hover: rgba(147,161,161,0.15);
    --error-fg: #dc322f; --error-bg: rgba(220,50,47,0.08);
    /* Code-block token colors (syntect classed spans; see --syn-* rules). */
    --syn-keyword: #859900; --syn-string: #2aa198; --syn-comment: #93a1a1;
    --syn-constant: #d33682; --syn-function: #268bd2; --syn-type: #b58900;
    /* Font stacks. The settings page only sets the *-choice variable
       inline when the user picks a family (see applyFonts); the var()
       fallback plus the rest of the stack keep missing glyphs and
       uninstalled names readable. */
    --reader-font: var(--reader-choice, "Segoe UI"), "Segoe UI Variable Text", system-ui, sans-serif;
    --mono-font: var(--mono-choice, Consolas), "Cascadia Mono", monospace;
    /* Base font size shared by the rendered and source panes (the
       Settings page overrides it inline); everything inside them is
       sized in em/% so it scales along. */
    --font-size: 15.5px;
  }
  :root[data-theme="latte"] {
    color-scheme: light;
    --bg: #eff1f5; --text: #4c4f69; --muted: #6c6f85; --border: #dce0e8;
    --link: #1e66f5; --code-bg: rgba(140,143,168,0.15); --code-block-bg: #e6e9ef;
    --th-bg: #e6e9ef; --hover: rgba(140,143,168,0.12);
    --error-fg: #d20f39; --error-bg: rgba(210,15,57,0.08);
    --syn-keyword: #8839ef; --syn-string: #40a02f; --syn-comment: #8c8fa1;
    --syn-constant: #fe640b; --syn-function: #1e66f5; --syn-type: #df8e1d;
  }
  :root[data-theme="one-dark"] {
    color-scheme: dark;
    --bg: #282c34; --text: #abb2bf; --muted: #7f8792; --border: #3e4451;
    --link: #61afef; --code-bg: rgba(171,178,191,0.15); --code-block-bg: #2c313a;
    --th-bg: #2c313a; --hover: rgba(171,178,191,0.12);
    --error-fg: #e06c75; --error-bg: rgba(224,108,117,0.12);
    --syn-keyword: #c678dd; --syn-string: #98c379; --syn-comment: #5c6370;
    --syn-constant: #d19a66; --syn-function: #61afef; --syn-type: #e5c07b;
  }
  :root[data-theme="mocha"] {
    color-scheme: dark;
    --bg: #1e1e2e; --text: #cdd6f4; --muted: #a6adc8; --border: #313244;
    --link: #89b4fa; --code-bg: rgba(205,214,244,0.10); --code-block-bg: #181825;
    --th-bg: #181825; --hover: rgba(205,214,244,0.08);
    --error-fg: #f38ba8; --error-bg: rgba(243,139,168,0.12);
    --syn-keyword: #cba6f7; --syn-string: #a6e3a1; --syn-comment: #6c7086;
    --syn-constant: #fab387; --syn-function: #89b4fa; --syn-type: #f9e2af;
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
  #ctx-menu button {
    display: flex; width: 100%; align-items: center; justify-content: space-between;
    font: 13px "Segoe UI", system-ui, sans-serif; color: var(--text);
    background: transparent; border: 0; border-radius: 5px; padding: 6px 10px; cursor: pointer;
  }
  /* The `display: flex` above out-specifies the UA's `[hidden] { display: none }`;
     without this, Cut/Paste/Select All would show in the read-only rendered pane. */
  #ctx-menu button[hidden] { display: none; }
  #ctx-menu button:disabled { color: var(--muted); cursor: default; }
  #ctx-menu button:hover:not(:disabled) { background: var(--hover); }

  /* ---- dialogs (close prompt, settings) ---- */
  #close-prompt, #settings {
    display: none; position: fixed; inset: 0; z-index: 30;
    align-items: center; justify-content: center;
    background: rgba(0,0,0,0.35);
  }
  #close-prompt .dialog, #settings .dialog {
    min-width: 320px; max-width: 440px; padding: 18px 20px 16px;
    background: var(--bg); border: 1px solid var(--border);
    border-radius: 10px; box-shadow: 0 12px 40px rgba(0,0,0,0.3);
  }
  #close-prompt .dialog-title, #settings .dialog-title {
    font-size: 15px; font-weight: 600; margin-bottom: 6px;
  }
  #close-prompt .dialog-body {
    font-size: 13px; color: var(--muted); margin-bottom: 16px;
    overflow-wrap: anywhere;
  }
  .dialog-actions { display: flex; justify-content: flex-end; gap: 8px; }

  /* Settings rows: label left, control right; changes apply live.
     Group captions divide theme from font settings. */
  #settings .setting-row {
    display: flex; align-items: center; justify-content: space-between;
    gap: 16px; margin: 12px 0;
  }
  #settings .setting-group {
    margin: 16px 0 2px; font-size: 11px; font-weight: 600;
    letter-spacing: .08em; text-transform: uppercase; color: var(--muted);
    border-bottom: 1px solid var(--border); padding-bottom: 5px;
  }
  #settings .setting-group:first-of-type { margin-top: 10px; }
  #settings label { font-size: 13px; }
  /* The size slider: thumb tracks the theme's link color; the readout is
     fixed-width so dragging does not make it dance. */
  #settings input[type="range"] {
    accent-color: var(--link); width: 130px; cursor: pointer;
  }
  #settings .size-control {
    display: flex; align-items: center; gap: 10px;
    font: 12px Consolas, monospace; color: var(--muted);
  }
  #settings .size-control span { min-width: 52px; text-align: right; }
  #settings select {
    font: 13px "Segoe UI", system-ui, sans-serif; color: var(--text);
    background: var(--bg); border: 1px solid var(--border); border-radius: 6px;
    padding: 5px 8px; min-width: 190px; cursor: pointer;
  }

  /* Buttons shared by the dialogs and the update toast. */
  #close-prompt button, #settings button, #update button {
    font: 13px "Segoe UI", system-ui, sans-serif; color: var(--text);
    background: transparent; border: 1px solid var(--border); border-radius: 6px;
    padding: 5px 14px; cursor: pointer;
  }
  #close-prompt button:hover, #settings button:hover, #update button:hover {
    background: var(--hover);
  }
  #close-prompt button.primary, #settings button.primary, #update button.primary {
    background: var(--link); border-color: var(--link); color: #fff;
  }
  #close-prompt button.primary:hover, #settings button.primary:hover,
  #update button.primary:hover {
    background: var(--link); filter: brightness(1.12);
  }

  /* ---- update toast ---- */
  /* Single row, nothing wraps; setUpdating swaps the offer for progress. */
  #update {
    display: none; position: fixed; right: 12px; bottom: 38px; z-index: 25;
    padding: 10px 14px;
    background: var(--bg); border: 1px solid var(--border); border-radius: 10px;
    box-shadow: 0 8px 24px rgba(0,0,0,0.25); font-size: 13px; white-space: nowrap;
  }
  #update-offer, #update-progress { display: flex; align-items: center; gap: 10px; }
  #update-progress { display: none; }
  #update a { color: var(--link); text-decoration: none; }
  #update a:hover { text-decoration: underline; }
  #update .spinner {
    width: 14px; height: 14px; border: 2px solid var(--border);
    border-top-color: var(--link); border-radius: 50%;
    animation: pound-spin 0.8s linear infinite;
  }
  @keyframes pound-spin { to { transform: rotate(360deg); } }
  #update-progress-line {
    display: inline-block; max-width: 340px;
    overflow: hidden; text-overflow: ellipsis; vertical-align: middle;
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
  /* A plain-text file IS its own source: the editable pane is the whole
     view — no rendered pane, no Source toggle (nothing to toggle). The
     leftover `split` class is overridden rather than removed, so the
     toggle state survives when a markdown file is opened next. */
  body.plain #content-wrap { display: none; }
  body.plain #source-wrap { display: block; }
  body.plain #topbar .toggle { display: none; }
  /* The source pane is a plain textarea: a real editor with native caret,
     selection and undo. It is never rewritten during editing (pushes go to
     pound.setRendered) so that state survives every keystroke. */
  #source {
    display: block; width: 100%; height: 100%; margin: 0;
    padding: 14px 18px 60px; border: 0; outline: none; resize: none;
    background: transparent; color: var(--text);
    font-family: var(--mono-font); font-size: var(--font-size); line-height: 1.6;
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
    font-family: var(--mono-font); font-size: 12.5px;
  }

  /* ---- markdown content (Solarized Light / One Dark palettes) ---- */
  #content {
    /* Full pane width like VSCode's preview (no centered reading column). */
    padding: 26px 40px 120px;
    font-family: var(--reader-font);
    font-size: var(--font-size); line-height: 1.65;
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
    font-family: var(--mono-font); font-size: 85%;
  }
  #content pre {
    position: relative; background: var(--code-block-bg); border-radius: 6px;
    padding: 14px 16px; margin: 0 0 16px; overflow: auto; line-height: 1.45;
  }
  #content pre code { background: none; padding: 0; font-size: 85%; }
  /* Syntax highlighting: syntect emits classed spans ("storage type",
     "constant numeric", …); the per-palette --syn-* vars above color
     them. Single-token selectors match any scope containing the token,
     so .type comes first ("storage type" keywords keep the keyword
     color) and function colors need token pairs — `meta function`
     wraps whole function bodies and must not match. */
  #content pre .type { color: var(--syn-type); }
  #content pre .keyword, #content pre .storage { color: var(--syn-keyword); }
  #content pre .string { color: var(--syn-string); }
  #content pre .comment { color: var(--syn-comment); font-style: italic; }
  #content pre .constant { color: var(--syn-constant); }
  #content pre .entity.function,
  #content pre .support.function,
  #content pre .variable.function { color: var(--syn-function); }
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
    font: 85% var(--mono-font); border: 1px solid var(--border);
    border-radius: 4px; padding: 1px 5px; background: var(--code-block-bg);
  }
  #content del { color: var(--muted); }
</style>
<script>
  // Resolve the theme before first paint so the app never flashes the
  // wrong one: the host injects the persisted settings as
  // window.poundSettings (see shell_html in view.rs). Auto follows the
  // system; each mode then uses its chosen palette. The chosen font is
  // applied for the same reason (the body's applyFonts re-runs and owns
  // the full logic).
  window.poundSettings = { mode: 'auto', light: 'solarized', dark: 'one-dark', font: '', size: 15.5 };
  window.poundFonts = [];
  (function () {
    var s = window.poundSettings;
    var mode = s.mode;
    if (mode === 'auto') {
      mode = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
    }
    document.documentElement.dataset.theme = mode === 'dark' ? s.dark : s.light;
    // Only the family choice is set here; the fallback stacks live once
    // in the :root CSS (--reader-font / --mono-font compose it). The
    // size needs no conditionality — :root already defaults it.
    var style = document.documentElement.style;
    if (s.font) {
      var choice = '"' + s.font + '"';
      style.setProperty('--reader-choice', choice);
      style.setProperty('--mono-choice', choice);
    }
    style.setProperty('--font-size', s.size + 'px');
  })();
</script>
</head>
<body>
  <div id="topbar">
    <span class="brand">Pound</span>
    <span class="spacer"></span>
    <button id="settings-open" type="button">Settings</button>
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
  <div id="ctx-menu">
    <button id="ctx-cut" type="button">Cut <kbd>Ctrl+X</kbd></button>
    <button id="ctx-copy" type="button">Copy <kbd>Ctrl+C</kbd></button>
    <button id="ctx-paste" type="button">Paste <kbd>Ctrl+V</kbd></button>
    <button id="ctx-selectall" type="button">Select All <kbd>Ctrl+A</kbd></button>
  </div>
  <div id="update">
    <div id="update-offer">
      <span>Pound <b id="update-version"></b> is available.</span>
      <a id="update-notes" href="">What's new</a>
      <button id="update-now" class="primary" type="button">Update</button>
      <button id="update-dismiss" type="button">Later</button>
    </div>
    <div id="update-progress">
      <div class="spinner"></div>
      <span id="update-progress-line">Updating&#8230;</span>
    </div>
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
  <div id="settings">
    <div class="dialog">
      <div class="dialog-title">Settings</div>
      <div class="setting-group">Theme</div>
      <div class="setting-row">
        <label for="setting-mode">Mode</label>
        <select id="setting-mode">
          <option value="auto">Auto (follow system)</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
      </div>
      <div class="setting-row">
        <label for="setting-light">Light palette</label>
        <select id="setting-light">
          <option value="solarized">Solarized Light</option>
          <option value="latte">Catppuccin Latte</option>
        </select>
      </div>
      <div class="setting-row">
        <label for="setting-dark">Dark palette</label>
        <select id="setting-dark">
          <option value="one-dark">One Dark</option>
          <option value="mocha">Catppuccin Mocha</option>
        </select>
      </div>
      <div class="setting-group">Font</div>
      <div class="setting-row">
        <label for="setting-font">Typeface</label>
        <select id="setting-font">
          <option value="">System default</option>
        </select>
      </div>
      <div class="setting-row">
        <label for="setting-size">Size</label>
        <span class="size-control">
          <input type="range" id="setting-size" min="10" max="28" step="0.5" value="15.5">
          <span id="setting-size-value">15.5 px</span>
        </span>
      </div>
      <div class="dialog-actions">
        <button id="settings-close" class="primary" type="button">Close</button>
      </div>
    </div>
  </div>
<script>
  window.pound = {
    setContent(html, source, title, path, status, edited, plain) {
      document.body.classList.toggle('plain', plain);
      const wrap = document.getElementById('content-wrap');
      const scroll = wrap.scrollTop;
      const content = document.getElementById('content');
      const welcome = document.getElementById('welcome');
      content.innerHTML = html;
      content.style.display = html ? 'block' : 'none';
      welcome.style.display = html || plain ? 'none' : 'flex';
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
      document.getElementById('update').style.display = 'block';
    },
    // The installer is running: lock the editor (the file on disk is about
    // to be replaced) and mirror the installer's progress lines.
    setUpdating() {
      setSaveEnabled(false);
      sourceEl.disabled = true;
      document.getElementById('update-offer').style.display = 'none';
      document.getElementById('update-progress').style.display = 'flex';
    },
    setUpdatingProgress(line) {
      document.getElementById('update-progress-line').textContent =
        line.replace(/^===>\s*/, '');
    },
    // Host-side clipboard text (the editor's Paste action): insert at
    // the caret like typing, so it lands on the native undo stack and
    // fires the usual edit round-trip.
    setClipboardText(text) {
      if (!text) return;
      sourceEl.focus();
      document.execCommand('insertText', false, text);
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
    // Plain files show only the source pane; there is nothing to toggle.
    // Drop the split class too, so a later markdown file does not open
    // with a source pane its (unchecked) toggle disagrees with.
    if (document.body.classList.contains('plain')) {
      toggle.checked = false;
      document.body.classList.remove('split');
      return;
    }
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

  // ---- links in the rendered pane ---------------------------------------
  // Clicks never navigate the reader (the browser resolves hrefs against
  // about:blank, mangling them beyond repair): the raw href goes to the
  // host, which opens web links externally and file targets in a new
  // Pound window. In-page anchors scroll within the document.
  document.getElementById('content').addEventListener('click', e => {
    const a = e.target.closest('a');
    if (!a) return;
    e.preventDefault();
    const href = a.getAttribute('href');
    if (!href) return; // sanitizer-stripped href: nothing to open
    if (href.startsWith('#')) {
      try {
        document.getElementById(decodeURIComponent(href.slice(1)))
                ?.scrollIntoView({ behavior: 'smooth' });
      } catch (_) { /* a malformed anchor id scrolls nowhere */ }
      return;
    }
    hostSend('link\n' + href);
  });

  // ---- settings ---------------------------------------------------------
  // Theme mode + one palette per mode (Zed-style), plus one typeface from
  // the installed families (window.poundFonts) applied to every reading
  // and editing surface. Every change applies live and is persisted by
  // the host; there is no OK/Cancel by design.
  const settings = Object.assign(
    { mode: 'auto', light: 'solarized', dark: 'one-dark', font: '', size: 15.5 },
    window.poundSettings
  );
  // applyFonts sets or clears the *-choice variables; the fallback
  // stacks live once, in the :root CSS that composes them.
  const quote = name => '"' + name.replace(/"/g, '\\"') + '"';
  function applyFonts() {
    const style = document.documentElement.style;
    const apply = (choice, name) =>
      name
        ? style.setProperty(choice, quote(name))
        : style.removeProperty(choice);
    apply('--reader-choice', settings.font);
    apply('--mono-choice', settings.font);
  }
  // One px size shared by both panes (they were hardcoded to mismatched
  // sizes — 12.5px source vs 15.5px rendered — before this setting).
  const applyFontSize = () =>
    document.documentElement.style.setProperty('--font-size', settings.size + 'px');
  function applyTheme() {
    let mode = settings.mode;
    if (mode === 'auto') {
      mode = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
    }
    document.documentElement.dataset.theme = mode === 'dark' ? settings.dark : settings.light;
  }
  const bindSetting = (id, key, name) => {
    const select = document.getElementById(id);
    select.value = settings[key];
    select.onchange = () => {
      settings[key] = select.value;
      applyTheme();
      applyFonts();
      hostSend('setting\n' + name + ' ' + select.value);
    };
  };
  bindSetting('setting-mode', 'mode', 'theme');
  bindSetting('setting-light', 'light', 'light');
  bindSetting('setting-dark', 'dark', 'dark');
  bindSetting('setting-font', 'font', 'font');
  // The slider applies live while dragging ('input' fires per step) and
  // persists once on release ('change').
  const sizeInput = document.getElementById('setting-size');
  const sizeValue = document.getElementById('setting-size-value');
  sizeInput.value = settings.size;
  sizeValue.textContent = settings.size + ' px';
  sizeInput.addEventListener('input', () => {
    settings.size = sizeInput.value;
    sizeValue.textContent = settings.size + ' px';
    applyFontSize();
  });
  sizeInput.addEventListener('change', () => {
    hostSend('setting\nsize ' + settings.size);
  });
  // Fill the font select from the host's enumeration; each option
  // previews in its own family, and a persisted-but-uninstalled font
  // (the host already dropped it from poundSettings) shows as default.
  const fillFontSelect = (id, key, list) => {
    const select = document.getElementById(id);
    for (const name of list) {
      const opt = document.createElement('option');
      opt.value = opt.textContent = name;
      opt.style.fontFamily = quote(name);
      select.appendChild(opt);
    }
    // Re-select after the options exist (a missing name falls back to
    // the "System default" option, value '').
    select.value = settings[key];
  };
  fillFontSelect('setting-font', 'font', window.poundFonts);

  const settingsDialog = document.getElementById('settings');
  const hideSettings = () => (settingsDialog.style.display = 'none');
  document.getElementById('settings-open').onclick = () =>
    (settingsDialog.style.display = 'flex');
  document.getElementById('settings-close').onclick = hideSettings;
  settingsDialog.addEventListener('click', e => {
    if (e.target === settingsDialog) hideSettings(); // click on the backdrop
  });
  addEventListener('keydown', e => { if (e.key === 'Escape') hideSettings(); });
  // While on Auto, follow live system theme changes.
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if (settings.mode === 'auto') applyTheme();
  });

  // ---- context menu --------------------------------------------------
  // Minimal replacement for the disabled WebView2 default menu. In the
  // editor pane it offers the clipboard set (Cut/Copy/Paste/Select All,
  // the clipboard-bound ones disabled without a selection); anywhere
  // else it stays Copy-only, shown when something is selected.
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
  const ctxButton = id => document.getElementById(id);
  document.addEventListener('contextmenu', e => {
    e.preventDefault();
    const editor = e.target === sourceEl;
    const hasSelection = selectedText().length > 0;
    if (!editor && !hasSelection) { hideCtxMenu(); return; }
    ['ctx-cut', 'ctx-paste', 'ctx-selectall'].forEach(id =>
      (ctxButton(id).hidden = !editor));
    ['ctx-cut', 'ctx-copy'].forEach(id =>
      (ctxButton(id).disabled = !hasSelection));
    ctxMenu.classList.add('show');
    // Open at the cursor, nudged back inside the window if it would overflow.
    ctxMenu.style.left = Math.min(e.clientX, innerWidth - ctxMenu.offsetWidth - 4) + 'px';
    ctxMenu.style.top = Math.min(e.clientY, innerHeight - ctxMenu.offsetHeight - 4) + 'px';
  });
  ctxButton('ctx-copy').onclick = () => {
    copyText(selectedText());
    hideCtxMenu();
  };
  // execCommand keeps Cut on the native undo stack (and fires the input
  // event, so the edit round-trip runs); the fallback covers browsers
  // where it is unavailable.
  ctxButton('ctx-cut').onclick = () => {
    sourceEl.focus();
    if (!document.execCommand('cut')) {
      copyText(selectedText());
      sourceEl.setRangeText('', sourceEl.selectionStart, sourceEl.selectionEnd, 'end');
      hostSend('edit\n' + sourceEl.value);
    }
    hideCtxMenu();
  };
  // WebView2 cannot read the clipboard from JS: ask the host, which
  // replies through pound.setClipboardText.
  ctxButton('ctx-paste').onclick = () => {
    hideCtxMenu();
    hostSend('clipboard-read');
  };
  ctxButton('ctx-selectall').onclick = () => {
    sourceEl.focus();
    sourceEl.select();
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
  // "Update" keeps the toast visible: setUpdating (host-driven) swaps it
  // to progress mode while the installer runs.
  document.getElementById('update-now').onclick = () => hostSend('update');
  document.getElementById('update-dismiss').onclick = () =>
    (document.getElementById('update').style.display = 'none');

  // Tell the host this shell has parsed and pound.setContent is callable:
  // the initial content push races the page load, so the host re-sends on
  // 'ready' (window.ipc exists only inside the app — in a plain browser
  // this is a no-op, keeping the preview recipe usable).
  if (window.ipc) window.ipc.postMessage('ready');
</script>
</body>
</html>
"#;
