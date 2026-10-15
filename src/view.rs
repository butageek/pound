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
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tao::dpi::{LogicalSize, PhysicalSize, Size};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::{http, DragDropEvent, WebView, WebViewBuilder, WebViewBuilderExtWindows};

use crate::fonts;
use crate::ipc::{self, json_str};
use crate::model::{render_source, Document, FileType};
use crate::presenter::{link_action, LinkAction, Presenter};
use crate::register;
use crate::theme::{DarkPalette, LightPalette, Settings, Theme};
use crate::update::{self, plan_exit, ClosePromptAnswer, ExitPlan};

/// The shell with the persisted settings and the installed font list
/// injected (`window.poundSettings` / `window.poundFonts`): the head
/// script resolves the theme and font before first paint, so the app
/// never flashes the wrong ones. `loading` adds the body class that
/// keeps the welcome screen hidden while the initial file's background
/// load is in flight (removed by the first `pound.setContent`).
fn shell_html(settings: &Settings, fonts: &[String], loading: bool) -> String {
    const PLACEHOLDER: &str =
        "window.poundSettings = { mode: 'auto', light: 'solarized', dark: 'one-dark', font: '', size: 15.5 };";
    const FONTS_PLACEHOLDER: &str = "window.poundFonts = [];";
    const BODY_PLACEHOLDER: &str = r#"<body data-mode="markdown" class="no-doc">"#;
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
    let body_injected = if loading {
        r#"<body data-mode="markdown" class="no-doc loading">"#
    } else {
        BODY_PLACEHOLDER
    };
    SHELL_HTML
        .replace(PLACEHOLDER, &settings_injected)
        .replace(FONTS_PLACEHOLDER, &fonts_injected)
        .replace(BODY_PLACEHOLDER, body_injected)
}

// ---- background rendering -------------------------------------------
// Every file load and every debounced re-render runs on one worker
// thread: rendering a large markdown or yaml file takes hundreds of ms
// (syntect + ammonia), and doing that on the UI thread froze the window
// — at startup it delayed the webview itself, during typing it stalled
// the event loop. Results come back as tao user events so the loop
// wakes the moment one lands (not on the next poll tick).

/// A unit of work for the render worker. `gen` is the document
/// generation the job belongs to: every open request bumps the
/// generation, and results from an older one are dropped (a load that
/// finishes after the user opened something else never replaces it).
enum Job {
    /// Load and render a whole file (initial open, drop, disk reload).
    Open {
        gen: u64,
        path: PathBuf,
        /// True when this load backs an auto-reload (the result adopts
        /// with reload semantics: identical content is a no-op, unsaved
        /// edits are never clobbered).
        reload: bool,
    },
    /// Re-render the current document's buffer (a debounced edit).
    Render {
        gen: u64,
        source: String,
        spec: RenderSpec,
    },
}

/// Everything a background render needs: the renderer dispatch (file
/// type) and the document's directory (the base for image references).
struct RenderSpec {
    file_type: FileType,
    base_dir: PathBuf,
}

/// A finished job, delivered to the event loop as a user event.
enum RenderEvent {
    Opened {
        gen: u64,
        reload: bool,
        result: Result<Document, String>,
    },
    Rendered {
        gen: u64,
        html: String,
    },
}

/// From a batch of already-queued jobs, keep only what still matters:
/// the newest Open, and the newest Render queued after it. The older
/// jobs' results would be dropped by the generation check anyway —
/// skipping them keeps a slow render from falling behind fast typing.
fn collapse(jobs: Vec<Job>) -> Vec<Job> {
    let mut open_at: Option<usize> = None;
    let mut render_at: Option<usize> = None;
    for (i, job) in jobs.iter().enumerate() {
        match job {
            Job::Open { .. } => {
                open_at = Some(i);
                render_at = None;
            }
            Job::Render { .. } => render_at = Some(i),
        }
    }
    jobs.into_iter()
        .enumerate()
        .filter(|(i, _)| open_at == Some(*i) || render_at == Some(*i))
        .map(|(_, job)| job)
        .collect()
}

/// Spawn the render worker (see the module note above). The thread ends
/// with the job channel when the loop (and its senders) go away.
fn spawn_render_worker(rx: std::sync::mpsc::Receiver<Job>, proxy: EventLoopProxy<RenderEvent>) {
    std::thread::spawn(move || {
        // Warm syntect's syntax set here (tens of ms, once per process):
        // neither the first file's load nor the UI thread pays it later.
        crate::markdown::warm_syntax_set();
        while let Ok(job) = rx.recv() {
            let mut batch = vec![job];
            while let Ok(next) = rx.try_recv() {
                batch.push(next);
            }
            for job in collapse(batch) {
                match job {
                    Job::Open { gen, path, reload } => {
                        let result = Document::load(&path);
                        let _ = proxy.send_event(RenderEvent::Opened {
                            gen,
                            reload,
                            result,
                        });
                    }
                    Job::Render { gen, source, spec } => {
                        let html = render_source(&source, spec.file_type, &spec.base_dir);
                        let _ = proxy.send_event(RenderEvent::Rendered { gen, html });
                    }
                }
            }
        }
    });
}

/// Request a background load of `path` (open, drop, reload): bump the
/// generation (older in-flight results become stale) and mark one in
/// flight (the disk poll must not stack another).
fn request_open(
    jobs: &Arc<Mutex<std::sync::mpsc::Sender<Job>>>,
    gen: &Cell<u64>,
    open_in_flight: &Cell<bool>,
    path: PathBuf,
    reload: bool,
) {
    let next = gen.get() + 1;
    gen.set(next);
    open_in_flight.set(true);
    let _ = jobs.lock().unwrap().send(Job::Open {
        gen: next,
        path,
        reload,
    });
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
    // The event loop carries the render worker's results as user events:
    // the loop wakes the instant one lands, not on the next poll tick.
    let event_loop = EventLoopBuilder::<RenderEvent>::with_user_event().build();
    // Background render worker (loads and debounced re-renders — see the
    // module note above).
    let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
    spawn_render_worker(job_rx, event_loop.create_proxy());
    let jobs: Arc<Mutex<std::sync::mpsc::Sender<Job>>> = Arc::new(Mutex::new(job_tx));

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

    let shared = Rc::new(RefCell::new(Presenter::new(None)));
    let dirty = Rc::new(Cell::new(false));
    // Set when a push must refresh only the preview (edit renders):
    // rewriting the textarea would destroy the caret and scroll position.
    let preview = Rc::new(Cell::new(false));
    // Set when only the status bar / save state / error banner changed
    // (banner-only events: a full document push would re-send megabytes
    // of html and rewrite the textarea for no visible reason).
    let status_only = Rc::new(Cell::new(false));
    // Set when the user confirmed an exit (ipc handlers run inside WebView2
    // message dispatch and cannot touch ControlFlow themselves).
    let exit = Rc::new(Cell::new(false));
    // The document generation: bumped on every open request; worker
    // results carrying an older generation are stale and dropped.
    let gen = Rc::new(Cell::new(0u64));
    // True while the initial file's background load is in flight: the
    // welcome screen stays hidden (`body.loading`) and pushes stay
    // quiet — the settle delivers the first real content.
    let loading = Rc::new(Cell::new(file.is_some()));
    // An open/reload is in flight (the disk poll must not stack another).
    let open_in_flight = Rc::new(Cell::new(false));
    // Whether the shell's 'ready' handshake ever arrived. WebView2 can
    // drop both the eager content push and the first 'ready' post to a
    // startup race (seen as an empty window over a loaded document);
    // the watchdog below re-pushes until the shell answers.
    let ready_seen = Rc::new(Cell::new(false));
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
    // The export's success toast (same delivery pattern): the written
    // file's display path, shown by `pound.setExported` below.
    let export_toast: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

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
        .with_html(shell_html(&settings, &fonts, loading.get()))
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
    // Banner-only changes (dismissed errors, refused links) need a
    // status push, not a full re-send of the document.
    let nav_status = status_only.clone();
    // The navigation handler's first call is the shell document's own
    // load — allowed whatever URL the platform reports (see link_action).
    let nav_initial = Rc::new(Cell::new(true));
    // Startup handshake: the eager `push_document` below races WebView2's
    // `NavigateToString` commit — when it runs while the initial blank
    // document is still current, `pound` is undefined and the push is lost
    // (wry ignores script exceptions). The shell therefore posts `ready`
    // once its scripts have parsed (see SHELL_HTML) and we re-push then.
    // The full catalog — every message kind the shell posts here and what
    // the host does with it — lives in `ipc::Message` (ipc.rs).
    let ipc_presenter = shared.clone();
    let ipc_dirty = dirty.clone();
    let ipc_preview = preview.clone();
    let ipc_status = status_only.clone();
    let ipc_exit = exit.clone();
    let ipc_jobs = Arc::clone(&jobs);
    let ipc_gen = gen.clone();
    let ipc_update_on_exit = update_on_exit.clone();
    let ipc_show_close_prompt = show_close_prompt.clone();
    let ipc_update_ui = show_update_progress.clone();
    let ipc_installer = Arc::clone(&installer);
    let ipc_clipboard_reply = Arc::clone(&clipboard_reply);
    let ipc_export_toast = Arc::clone(&export_toast);
    let ipc_theme_changed = theme_changed.clone();
    let ipc_ready_seen = ready_seen.clone();
    let ipc_loading = loading.clone();
    // Font list for validating settings-page saves (see fonts::families).
    let ipc_fonts = fonts.clone();
    let builder = builder.with_ipc_handler(move |request| {
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
                    ipc_presenter
                        .borrow_mut()
                        .fail(format!("could not start the updater: {e}"));
                    ipc_status.set(true);
                    false
                }
            }
        };
        // The wire protocol (message kinds, payload shapes) lives in
        // ipc.rs so it is unit-testable; anything unrecognized is
        // ignored, exactly as the old if/else chain did.
        let Some(message) = ipc::parse(request.body()) else {
            return;
        };
        match message {
            ipc::Message::Ready => {
                ipc_ready_seen.set(true);
                // Nothing to push while the initial file is still loading
                // (the settle pushes); otherwise the shell answered late
                // and must be fed the current state.
                if !ipc_loading.get() {
                    ipc_dirty.set(true);
                }
            }
            ipc::Message::Save => {
                ipc_presenter.borrow_mut().save();
                // The html did not change: status/save state only.
                ipc_status.set(true);
            }
            ipc::Message::ExportHtml => {
                // Success: the toast announces the file (delivered in
                // MainEventsCleared — ipc handlers cannot evaluate
                // scripts); failure or no-op falls to the status push,
                // which shows the banner. The document is untouched.
                match ipc_presenter.borrow_mut().export_html() {
                    Some(target) => {
                        *ipc_export_toast.lock().unwrap() = Some(target.display().to_string());
                    }
                    None => ipc_status.set(true),
                }
            }
            ipc::Message::OpenExport { path } => {
                // The export toast's Open: hand the written file to the
                // system browser (same path as web links; a miss — the
                // file was deleted between export and click — banners).
                if !ipc_presenter.borrow_mut().open_externally(&path) {
                    ipc_status.set(true); // banner-only
                }
            }
            ipc::Message::SaveAndExit => {
                let open_path = ipc_presenter.borrow().document_path();
                let mut presenter = ipc_presenter.borrow_mut();
                presenter.save();
                let saved = !presenter.is_dirty();
                drop(presenter);
                // A plain close saves and exits; only the update flow
                // (its Update button armed the pending flag) may run the
                // installer on the way out. A failed save keeps the app
                // open with the banner.
                match plan_exit(
                    ClosePromptAnswer::Save { saved },
                    ipc_update_on_exit.replace(false),
                ) {
                    ExitPlan::KeepOpen => ipc_status.set(true),
                    ExitPlan::RunInstaller => {
                        // Progress mode: the installer closes us at the
                        // replace step; a failed spawn falls back to a
                        // plain close.
                        if !begin_update(open_path.as_deref()) {
                            ipc_exit.set(true);
                        }
                    }
                    ExitPlan::Exit => ipc_exit.set(true),
                }
            }
            ipc::Message::Exit => {
                // Same rule as SaveAndExit: discard-and-close exits
                // unless an update is actually pending.
                let open_path = ipc_presenter.borrow().document_path();
                match plan_exit(
                    ClosePromptAnswer::Discard,
                    ipc_update_on_exit.replace(false),
                ) {
                    ExitPlan::RunInstaller => {
                        if !begin_update(open_path.as_deref()) {
                            ipc_exit.set(true);
                        }
                    }
                    _ => ipc_exit.set(true),
                }
            }
            ipc::Message::Update => {
                if ipc_presenter.borrow().is_dirty() {
                    // Reuse the close prompt: save or discard, then update.
                    ipc_update_on_exit.set(true);
                    ipc_show_close_prompt.set(true);
                } else {
                    let open_path = ipc_presenter.borrow().document_path();
                    begin_update(open_path.as_deref());
                }
            }
            ipc::Message::CancelExit => ipc_update_on_exit.set(false),
            ipc::Message::Setting { name, value } => {
                // Validate before persisting; map to the registry value
                // name. The font accepts "" (back to the default stacks)
                // or any installed family; names are free-form and may
                // contain spaces, which the parser kept in the value.
                let (valid, reg_name, repaints) = match name.as_str() {
                    "theme" => (Theme::parse(&value).is_some(), "Theme", true),
                    "light" => (LightPalette::parse(&value).is_some(), "LightPalette", true),
                    "dark" => (DarkPalette::parse(&value).is_some(), "DarkPalette", true),
                    "font" => (
                        value.is_empty() || ipc_fonts.iter().any(|f| f == &value),
                        "Font",
                        false,
                    ),
                    "size" => (
                        crate::theme::parse_font_size(&value).is_some(),
                        "FontSize",
                        false,
                    ),
                    _ => (false, "", false),
                };
                if valid {
                    register::save_setting(reg_name, &value);
                    // Theme/palette changes must also repaint the
                    // pre-paint backgrounds (see the theme_changed flag).
                    if repaints {
                        ipc_theme_changed.set(true);
                    }
                }
            }
            ipc::Message::ClipboardRead => {
                // A non-text clipboard simply yields no reply: Paste no-ops.
                if let Some(text) = crate::clipboard::read_text() {
                    *ipc_clipboard_reply.lock().unwrap() = Some(text);
                }
            }
            ipc::Message::Link { href } => {
                // The shell intercepts link clicks (the browser resolves
                // hrefs against about:blank, mangling them before any
                // navigation handler could see them).
                match link_action(&href, false) {
                    LinkAction::OpenExternally => {
                        if !ipc_presenter.borrow_mut().open_externally(&href) {
                            ipc_status.set(true);
                        }
                    }
                    LinkAction::Refuse => {
                        // A local-file target: open it in a new Pound window,
                        // VSCode-style — a missing file shows its own error
                        // banner there instead of trapping this window.
                        if let Some(path) = ipc_presenter.borrow().link_target(&href) {
                            // A detached child: dropping the handle does not
                            // kill the process, so the window outlives us.
                            let spawned = std::env::current_exe()
                                .and_then(|exe| std::process::Command::new(exe).arg(&path).spawn());
                            if let Err(e) = spawned {
                                ipc_presenter.borrow_mut().fail(format!(
                                    "could not open a new window for {}: {e}",
                                    path.display()
                                ));
                                ipc_status.set(true);
                            }
                        }
                    }
                    // Anchors scroll in the shell; pound:// controls still
                    // travel through the navigation handler.
                    LinkAction::Stay | LinkAction::Internal => {}
                }
            }
            ipc::Message::Edit { text } => {
                // Buffer-first: the model stores the text immediately (the
                // title/status bar/save all act on the newest buffer) and
                // the render runs on the worker — its result arrives as a
                // user event and lands through pound.setRendered, which
                // never touches the textarea.
                let Some((file_type, base_dir)) = ipc_presenter.borrow().render_spec() else {
                    return; // no document: nothing to edit
                };
                if !ipc_presenter.borrow_mut().edit_buffer(&text) {
                    return; // unchanged buffer: no dirty marker, no render
                }
                // Status push now — dirty marker + fresh counts; the
                // rendered html follows with the worker's result.
                ipc_status.set(true);
                // Plain files render nothing (the source pane IS the
                // view): skip the round-trip.
                if file_type != FileType::Plain {
                    let _ = ipc_jobs.lock().unwrap().send(Job::Render {
                        gen: ipc_gen.get(),
                        spec: RenderSpec {
                            file_type,
                            base_dir,
                        },
                        source: text,
                    });
                }
            }
            ipc::Message::CsvSort { column } => {
                // Table-only refresh: same flow as an edit, minus the
                // (nonexistent) editor buffer to protect.
                if ipc_presenter.borrow_mut().sort_csv(column) {
                    ipc_preview.set(true);
                }
            }
        }
    });
    // File drops: WebView2 forwards them to the host only when a
    // drag-drop handler is registered (wry sets AllowExternalDrop(false)
    // and installs its own controller) — without one, the page silently
    // swallows them and tao's DroppedFile never fires because the webview
    // covers the whole client area. The pick (markdown, then CSV, then
    // the last path) loads on the worker; the previous document stays on
    // screen until the new one lands.
    let drop_presenter = shared.clone();
    let drop_jobs = Arc::clone(&jobs);
    let drop_gen = gen.clone();
    let drop_in_flight = open_in_flight.clone();
    let builder = builder.with_drag_drop_handler(move |event| {
        if let DragDropEvent::Drop { paths, .. } = event {
            if let Some(path) = drop_presenter.borrow().pick_dropped(&paths) {
                request_open(&drop_jobs, &drop_gen, &drop_in_flight, path, false);
            }
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
                &nav_status,
            )
        })
        .build(&window)
        .expect("create webview");

    // The initial file (if any) loads on the worker: the window and the
    // webview are already up, `body.loading` keeps the welcome screen
    // hidden, and the provisional title names the file while it loads.
    // The settle lands as a user event below and pushes the document.
    if let Some(path) = file {
        window.set_title(&provisional_title(&path));
        request_open(&jobs, &gen, &open_in_flight, path, false);
    } else {
        // No file: paint the welcome screen right away.
        push_document(&webview, &shared.borrow());
        window.set_title(&shared.borrow().title());
    }

    let mut last_pushed = shared.borrow().revision();
    let mut last_title = shared.borrow().title();
    let mut last_poll = Instant::now();
    // Window-size persistence, debounced: a live resize fires Resized
    // continuously, and a registry write per event is pure I/O churn.
    // The latest size is stashed on each event, written once resizing
    // goes quiet, and flushed on close (see flush_window_size).
    const RESIZE_QUIET: Duration = Duration::from_millis(500);
    let pending_size: Cell<Option<(u32, u32)>> = Cell::new(None);
    let last_resize = Cell::new(Instant::now());
    // Handshake watchdog: re-push the document on a ladder until 'ready'
    // arrives (harmless when the first push landed). Bounded so a truly
    // dead bridge logs off instead of spinning forever.
    const READY_RETRY_AFTER: Duration = Duration::from_millis(700);
    const READY_RETRY_EVERY: Duration = Duration::from_millis(500);
    const READY_RETRY_MAX: u32 = 20;
    let started = Instant::now();
    let mut ready_retry_at = started;
    let mut ready_retries = 0u32;
    // Update-progress UI state: the toast switched to progress mode, and
    // how many installer lines have been mirrored into it so far.
    let mut update_ui_active = false;
    let mut update_lines_shown = 0usize;

    event_loop.run(move |event, _, control_flow| {
        // Wake up periodically for the file-change poll.
        *control_flow = ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL);

        match event {
            // A background load or render finished. The generation check
            // drops anything stale (a load that raced a newer open, a
            // render of a document the user already replaced).
            Event::UserEvent(RenderEvent::Opened {
                gen: job_gen,
                reload,
                result,
            }) => {
                if job_gen == gen.get() {
                    open_in_flight.set(false);
                    loading.set(false);
                    if reload {
                        // Reload semantics: identical content is quiet,
                        // in-flight edits win (Model::adopt_reload).
                        if shared.borrow_mut().adopt_reload(result) {
                            dirty.set(true);
                        } else {
                            status_only.set(true);
                        }
                    } else {
                        shared.borrow_mut().adopt(result);
                        dirty.set(true);
                    }
                }
            }
            Event::UserEvent(RenderEvent::Rendered { gen: job_gen, html }) => {
                if job_gen == gen.get() && shared.borrow_mut().apply_render(html) {
                    preview.set(true);
                }
            }

            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // Unsaved edits: ask before quitting (the shell's dialog
                // posts save-and-exit/exit; cancel just closes it). While
                // an update is running the buffer is clean by definition —
                // the installer will close us at the replace step.
                let updating = installer.lock().unwrap().is_some();
                if shared.borrow().is_dirty() && !updating {
                    eval_script(&webview, "pound.setClosePrompt(true);".to_owned());
                } else {
                    *control_flow = ControlFlow::Exit;
                }
            }

            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                // Stash the size (debounced write — see MainEventsCleared)
                // so the next launch opens like this one. Skip maximized
                // windows: the restore size is the one worth keeping
                // (maximizing is not remembered itself).
                if !window.is_maximized() {
                    pending_size.set(Some((size.width, size.height)));
                    last_resize.set(Instant::now());
                }
            }

            Event::MainEventsCleared => {
                if exit.get() {
                    *control_flow = ControlFlow::Exit;
                }
                // The window went quiet after a resize: persist the
                // remembered size once (a write per Resized event was
                // continuous registry I/O during live drags).
                if pending_size.get().is_some() && last_resize.get().elapsed() >= RESIZE_QUIET {
                    flush_window_size(&pending_size);
                }
                if !ready_seen.get()
                    && !loading.get()
                    && started.elapsed() >= READY_RETRY_AFTER
                    && ready_retry_at.elapsed() >= READY_RETRY_EVERY
                    && ready_retries < READY_RETRY_MAX
                {
                    ready_retries += 1;
                    ready_retry_at = Instant::now();
                    push_document(&webview, &shared.borrow());
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
                // Deliver the export's success toast; the status line
                // gets the durable "exported to …" notice without a
                // full re-render (push_status leaves both panes alone —
                // no re-parse, no image reloads).
                if let Some(path) = export_toast.lock().unwrap().take() {
                    eval_script(&webview, format!("pound.setExported({});", json_str(&path)));
                    push_status(&webview, &shared.borrow());
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
                if last_poll.elapsed() >= POLL_INTERVAL && !open_in_flight.get() {
                    last_poll = Instant::now();
                    // Cheap mtime check only — when the disk moved, the
                    // reload itself (read + render) runs on the worker.
                    if shared.borrow_mut().poll_disk_changed() {
                        if let Some(path) = shared.borrow().document_path() {
                            request_open(&jobs, &gen, &open_in_flight, path, true);
                        }
                    }
                }
                let revision = shared.borrow().revision();
                let pushed = if preview.get() {
                    // Edit-render refresh: rendered pane + status only.
                    preview.set(false);
                    status_only.set(false); // covered by push_rendered
                    last_pushed = revision;
                    push_rendered(&webview, &shared.borrow());
                    true
                } else if dirty.get() || revision != last_pushed {
                    dirty.set(false);
                    status_only.set(false); // covered by push_document
                    last_pushed = revision;
                    push_document(&webview, &shared.borrow());
                    true
                } else if status_only.replace(false) {
                    // Banner/status-only refresh (dismissed error, save
                    // ack…): neither pane is touched — no re-parse, no
                    // image reloads, no textarea rewrite.
                    push_status(&webview, &shared.borrow());
                    true
                } else {
                    false
                };
                if pushed {
                    let title = shared.borrow().title();
                    if title != last_title {
                        last_title = title;
                        window.set_title(&last_title);
                    }
                }
            }

            // Every exit path funnels through here: persist a stashed
            // window size one last time (resize-then-close must not lose
            // it just because the debounce never went quiet).
            Event::LoopDestroyed => {
                flush_window_size(&pending_size);
            }

            _ => {}
        }
    });
}

/// Decide one navigation: `pound://` controls run their intent, web
/// links open externally, in-page anchors stay — everything else is
/// refused with a message. The rules, and why the reader never
/// navigates away from the shell document, live in [`link_action`]
/// (`initial_load` allows the shell's own first load). `status` is set
/// when a banner-only change needs a push (never a full re-send).
fn handle_navigation(
    url: &str,
    initial_load: bool,
    presenter: &mut Presenter,
    status: &Cell<bool>,
) -> bool {
    match link_action(url, initial_load) {
        LinkAction::Internal => {
            if let Some(command) = url.strip_prefix("pound://") {
                match command.split(['?', '#']).next().unwrap_or("") {
                    "dismiss-error" => {
                        presenter.dismiss_error();
                        status.set(true);
                    }
                    _ => {}
                }
            }
            false
        }
        LinkAction::OpenExternally => {
            if !presenter.open_externally(url) {
                status.set(true);
            }
            false
        }
        LinkAction::Refuse => {
            presenter.refuse_link(url);
            status.set(true);
            false
        }
        LinkAction::Stay => true,
    }
}

/// The window title while the initial file is still loading in the
/// background (the real title lands with the document).
fn provisional_title(path: &Path) -> String {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".to_owned());
    format!("{name} — Pound")
}

/// Persist a stashed window size (see the debounced Resized handler).
fn flush_window_size(pending: &Cell<Option<(u32, u32)>>) {
    if let Some((width, height)) = pending.take() {
        register::save_setting("WindowWidth", &width.to_string());
        register::save_setting("WindowHeight", &height.to_string());
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

/// Run a shell script. wry ignores JS exceptions (see the ready
/// handshake), but transport failures still surface on stderr.
fn eval_script(webview: &WebView, js: String) {
    if let Err(e) = webview.evaluate_script(&js) {
        eprintln!("pound: failed to update the view: {e}");
    }
}

/// Push the whole document into the page (document opened/reloaded):
/// both panes, status bar, save state, title — everything pulled through
/// the presenter's read accessors. The textarea is rewritten, so this
/// must never run for edit-driven updates.
fn push_document(webview: &WebView, presenter: &Presenter) {
    eval_script(
        webview,
        format!(
            "pound.setContent({}, {}, {}, {}, {}, {}, {}); pound.setError({});",
            json_str(presenter.html()),
            json_str(presenter.source()),
            json_str(&presenter.title()),
            json_str(&presenter.display_path()),
            json_str(&presenter.status_text()),
            presenter.is_dirty(),
            // String arguments must go through json_str: raw text would
            // evaluate as a JS identifier and throw — silently, since
            // wry ignores script exceptions — killing the whole push.
            json_str(presenter.view_mode()),
            json_str(presenter.error_text()),
        ),
    );
}

/// Push only the rendered pane, status bar and save state — the editor
/// buffer (textarea) is left untouched so the caret, selection, undo stack
/// and scroll position survive every keystroke round-trip.
fn push_rendered(webview: &WebView, presenter: &Presenter) {
    eval_script(
        webview,
        format!(
            "pound.setRendered({}, {}, {}); pound.setError({});",
            json_str(presenter.html()),
            json_str(&presenter.status_text()),
            presenter.is_dirty(),
            json_str(presenter.error_text()),
        ),
    );
}

/// Push only the status bar, save state and error banner — neither pane
/// is touched. For pushes that change no rendering (e.g. the export
/// notice): re-sending the HTML would re-parse the document and re-fire
/// every poundimg request for no visible change.
fn push_status(webview: &WebView, presenter: &Presenter) {
    eval_script(
        webview,
        format!(
            "pound.setStatus({}, {}); pound.setError({});",
            json_str(&presenter.status_text()),
            presenter.is_dirty(),
            json_str(presenter.error_text()),
        ),
    );
}

fn window_icon() -> Option<tao::window::Icon> {
    let decoded = image::load_from_memory(include_bytes!("../assets/pound.png")).ok()?;
    let rgba = decoded.to_rgba8();
    let (w, h) = rgba.dimensions();
    tao::window::Icon::from_rgba(rgba.into_raw(), w, h).ok()
}

// The shell document — top bar, rendered pane, source pane, status bar —
// lives in `shell.html` as a real file (editor tooling, reviewable diffs);
// include_str! embeds it in the binary at compile time.
const SHELL_HTML: &str = include_str!("shell.html");
