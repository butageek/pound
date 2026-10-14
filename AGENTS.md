# AGENTS.md — Pound playbook

Environment, build, release and debugging notes for humans *and* AI coding
agents. (Companion repo to [linkport](https://github.com/butageek/linkport);
same conventions.)

## What this is

Pound is a lightweight file viewer for Windows written in Rust (tao +
wry + pulldown-cmark). It started as a markdown reader — `.md` files
render by default with the raw source side by side (Ctrl+U), and the
source pane is a live editor: typing re-renders the preview and `Ctrl+S`
saves back to disk. Beyond markdown, `.csv`/`.tsv` files render as a
sortable, column-resizable table (no source pane — data, not prose),
and any other text file opens straight into the editor (a text file is
its own source). Architecture is strict MVP
(Model–View–Presenter) — keep it that way:

| Layer | File | Rule |
|---|---|---|
| Model | `src/model.rs` | state only, no UI types |
| Rendering input | `src/markdown.rs` | pulldown-cmark → sanitized HTML, unit-tested headlessly |
| Rendering input | `src/csv.rs` | delimited text → sanitized, sortable HTML table (delimiter sniffing, numeric-aware sort, 10k-row render cap), unit-tested headlessly |
| Rendering input | `src/export.rs` | markdown source → standalone HTML export (self-contained page: embedded CSS, `poundimg://` srcs rewritten to relative paths), unit-tested headlessly |
| Presenter | `src/presenter.rs` | intents/use-cases; owns the Model behind a **private field** — the view mutates only via intents (`open_externally`, `refuse_link`, `fail`, …) and reads only via accessors (`html()`, `is_dirty()`, `revision()`, …), never `model.*` directly; no GUI types |
| JS bridge | `src/ipc.rs` | the shell→host wire protocol (`ipc::Message`, `parse`) + host→shell string encoding (`json_str`); headless-tested |
| View | `src/view.rs` | WebView2 shell (tao + wry): window + event loop, ipc dispatch; embeds the shell document (`src/shell.html` via `include_str!`) |
| Windows glue | `src/register.rs` | HKCU registry: app-list registration + `.md`/`.csv`/`.tsv` associations |

## Environment

- Development happens on Linux/WSL; the shipped binary is Windows. The GUI
  (tao/wry) is `[target.'cfg(windows)'.dependencies]`-gated, so Linux
  hosts run `cargo test` only and cannot open the GUI.
- Windows type-checking needs the target: `rustup target add x86_64-pc-windows-msvc`
  (a plain `cargo check --target` needs no linker; the icon is only embedded
  in real builds). WebView2 ships with Windows 10/11.
- **Release builds use MSVC** (CI builds on `windows-latest`; locally needs
  VS Build Tools). NEVER build releases with windows-gnu/mingw: mingw cannot
  link Microsoft's static WebView2Loader lib, so the exe imports
  `WebView2Loader.dll` dynamically and dies at startup with
  STATUS_DLL_NOT_FOUND (0xC0000135) on user machines — an early release
  shipped exactly this way, which is why the release runner is MSVC.
- The logo & icon set — `assets/pound.ico` (16–256), `assets/pound.png`,
  and the SVG/PNG assets under `assets/logo/` — is regenerated with
  `python3 tools/gen_icon.py` (stdlib only; supersampled anti-aliasing).
  Design: "the four-color hash" — the pound sign as a grid, one accent
  color per stroke: one app, many kinds of content.
  It prints an ASCII preview so geometry can be checked from a terminal.

## Rendering notes

- The markdown pane is an **embedded WebView2** (`wry`) — the same class of
  browser engine VSCode's preview uses. Markdown → HTML (pulldown-cmark) →
  CSS; we deliberately do NOT hand-roll text layout (the egui era taught us
  baselines, code chips and tables are an endless whack-a-mole).
- Guiding principle: "the file's bytes are the truth; rendering is a
  layer; when rendering fails, fall back to source, never eat content."
- **`model::FileType` is the one source of truth** for rendering, pane
  layout and `type_label`: `Markdown` (`.md`/`.markdown`) renders the
  preview + editable source side by side; `Csv` (`.csv`/`.tsv`) renders
  a table-only view; everything else is `Plain` and renders **nothing**
  (`html` stays empty, edits never re-render) — the view shows the
  **editable source pane alone**, a text file being its own source: no
  second pane, no Source toggle, full editor right-click menu, inert
  scroll sync. The shell picks the layout from the mode string pushed
  with the document (`body[data-mode="markdown|plain|csv"]` CSS;
  `presenter::view_mode`).
- **CSV files (`csv.rs`)**: the delimiter is sniffed from the first
  line (`,` `;` tab `|`, outside quotes), the first row is the header,
  and cells are escaped + ammonia-sanitized (file content never
  executes; ammonia's default allowlist already covers the table set —
  only the `class` attrs are added). The view is the table alone:
  read-only, no source pane, Save and Source hidden. **Sorting** stays
  in the unidirectional flow — a header click posts
  `csv-sort\n<column>`, `Model::sort_csv` re-renders the stored grid
  under the new sort (same column toggles direction) and the usual
  preview push delivers it; numeric columns (every non-empty value
  parses) sort numerically, empties sink. **Column widths** are pure
  layout and live in the shell: each column opens at a content-fit
  default (the widest cell, canvas-measured in the table's fonts,
  clamped to 40–400 px; leftover pane width goes to uncapped columns
  so narrow files fill the pane). Dragging a header's edge resizes
  only that column (same bounds): every column carries its own px
  width and the table is exactly their sum — no percentage shares
  couple them. The widths + scroll survive the sort's re-render
  (`csvColWidths`, re-applied in `setupCsvTable` and pinned before the
  scroll is restored — an unpinned table fits the pane and would clamp
  the sideways position away); tables wider than the pane scroll
  horizontally with a sticky header. Table CSS is scoped per file type
  (`body[data-mode=…]`): markdown's `display:block`/`max-width`
  scrolling rule once leaked into the csv table and made the engine
  rescale every column — modes must not inherit each other's table
  box models. Files beyond 10,000 data
  rows render the cap plus a visible note; the status bar counts
  rows/columns (the file's true shape), never characters. Binary files
  (PDFs, .docx, ...) fail with an actionable message instead of Rust's
  UTF-8 error.
- **Syntax highlighting** happens in `markdown.rs` at render time:
  fenced blocks with a language syntect knows are highlighted into
  classed spans (`<span class="storage type …">`, `highlight_codeblocks`
  + syntect's `ClassedHTMLGenerator`, `ClassStyle::Spaced`); the colors
  live in the shell CSS (`--syn-*` vars per palette) so no colors are
  baked into the HTML and every palette themes code. Unknown languages,
  plain fences and indented blocks replay pulldown's plain escaped
  events (ammonia keeps sanitizing: `span`/`code` may carry only
  `class`). The fence info string's first token is the language
  (```rust,ignore → rust). syntect is built with `regex-fancy` (pure
  Rust, no oniguruma) and without the default themes dump.
- Rust → JS: `push_document` evaluates `pound.setContent(html, source,
  title, path, status)` + `pound.setError(msg)` — strings JSON-escaped by
  `json_str` (quoting only; XSS is handled by ammonia). The wire protocol
  itself — parsing the shell's ipc messages and encoding the host's
  replies — lives in `src/ipc.rs` (`ipc::Message`), unit-tested headlessly. `status` feeds the
  bottom status bar (path + file type + char/line counts — rows/columns
  for tables; `model.rs` strips
  the `\\?\` verbatim prefix `canonicalize` adds, so users see `C:\…`).
- JS → Rust: in-page controls navigate to `pound://dismiss-error` (error
  banner), intercepted by the navigation handler which calls presenter
  intents. There is deliberately no Open/Reload UI: files arrive by
  double-click, drag-and-drop or CLI, and on-disk edits auto-reload.
- Links never navigate the reader (VSCode-style): the shell intercepts
  clicks in `#content` and posts `link\n<raw href>` over ipc — the raw
  href, because the browser resolves hrefs against `about:blank` and
  mangles them before any navigation handler could see them
  (`about:blank/rel.md` is a *different* blank document). The host
  routes by `presenter::link_action`: web/mailto → external browser;
  file targets (`file:` URLs, absolute or relative paths, resolved
  against the document's directory by `Presenter::link_target` with
  percent-decoding) → a **new Pound window** (`current_exe` spawn),
  which reports a missing file in its own error banner; anchors scroll
  in-page. The navigation handler remains as a safety net: only the
  initial load and same-document `about:blank#…` anchors stay;
  everything else is refused. ammonia allows the `file:` scheme on
  hrefs for this path.
- Drag & drop only reaches the host when a wry `with_drag_drop_handler`
  is registered — that's what makes wry set `AllowExternalDrop(false)`;
  without it the WebView2 page silently swallows drops (tao's
  `DroppedFile` never fires, the webview covers the whole client area).
  The handler routes `DragDropEvent::Drop` paths into
  `presenter.open_dropped`; the return value is ignored on Windows.
- Startup handshake: `evaluate_script` right after `build` races
  WebView2's `NavigateToString` commit — a warm-started second instance
  regularly loses (the script runs in the still-blank document where
  `pound` is undefined; wry ignores the exception), which showed up as
  "opened a file into a new window, no content". The shell posts
  `window.ipc.postMessage('ready')` when its scripts parsed and the view
  re-pushes the document. `window.ipc` is injected only when
  `with_ipc_handler` is registered, so it's a no-op in plain browsers and
  the preview recipe below keeps working. **The race can eat BOTH
  directions** (observed with runtime 154: eager push lost AND 'ready'
  posts never delivered — an empty welcome window over a loaded
  document, correct title, dead ipc; intermittent, so it looked like a
  code regression for an evening). Belt and suspenders: the shell
  re-posts 'ready' at +250ms/+1s/+2.5s (idempotent host-side), and the
  view keeps a `ready_seen` flag with a watchdog that re-pushes the
  document every 500ms (after a 700ms grace, max 20 tries) until the
  handshake lands. Debug such states via
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222`
  + CDP (`/json/list` → WebSocket → `Runtime.evaluate`) — the window
  title shows whether the *document* loaded (Rust side) while the DOM
  shows whether the *push* landed.
- Local images: `markdown.rs` rewrites relative srcs to percent-encoded
  `poundimg://<abs path>`; the view serves them from disk via a custom
  protocol. Unknown/missing files keep their src (broken-image marker).
- Scroll sync (VSCode-style): `markdown.rs` wraps every top-level block
  in a `<div data-line-start data-line-end>` (byte ranges from pulldown's
  offset iter, binary-searched to 1-based line numbers). The shell maps
  each pane's scroll position to the matching line/block of the other,
  interpolating within a block; programmatic sets stamp a short per-pane
  echo-suppression window so the panes never ping-pong. `data-` survives
  ammonia via a generic attribute prefix (inert — scripts are stripped),
  and only direct children of `#content` count as sync anchors, so a
  file's raw HTML can't forge them.
- Editing (the source pane is a `<textarea>`): typing is debounced 250ms
  in the shell, then posted as `edit\n<buffer>` over `window.ipc`; the
  model re-renders (Rust owns rendering + ammonia) and the view answers
  with `pound.setRendered(html, status, edited)` — which NEVER rewrites
  the textarea, so caret/selection/undo/scroll survive every keystroke.
  Full `pound.setContent(...)` (textarea rewrite + caret parked at top)
  runs only for real document loads. `save` posts over the same channel;
  `model.save` preserves the file's CRLF/LF style (the DOM normalizes the
  buffer to LF).
- **Export HTML** (markdown only): the top bar's Export button
  (`Ctrl+E`) posts `export-html`; `Model::export_html` renders the
  *live buffer* (unsaved edits included — an export is what you see)
  through `export::standalone_html` and writes it next to the source
  (`notes.md` → `notes.html`; re-export overwrites). The page is
  self-contained: the same ammonia-sanitized, syntax-highlighted HTML
  as the preview, an embedded stylesheet (Solarized Light / One Dark
  switched by `prefers-color-scheme` — the app's palettes, no assets,
  no scripts), and local `poundimg://` image srcs rewritten back to
  paths *relative to the document's directory* (percent-encoded, `/`
  kept) so the export keeps its images wherever it moves — the
  renderer's own `markdown::percent_decode` powers the rewrite. Success
  reports in the status bar (`exported to notes.html`) via
  `Model::notice`, cleared by open/edit/save; failures take the usual
  error banner. Plain/CSV documents hide the button
  (`body[data-mode]` CSS, like Save/Source).
- Disk-conflict policy: unsaved edits pause auto-reload. The status bar
  shows `edited`, plus `changed on disk` when the file was modified
  externally (saving resolves it, last writer wins); a same-content mtime
  touch never reloads. The window title prefixes `*` while unsaved.
- Themes: a **Settings** page (the top bar's leftmost button) with the
  Zed-style model:
  the mode (Auto / Light / Dark) plus a palette per mode — light:
  Solarized or Catppuccin Latte; dark: One Dark or Catppuccin Mocha.
  Changes apply live and persist immediately
  (`HKCU\Software\Pound`: `Theme`, `LightPalette`, `DarkPalette` —
  `theme.rs` + `register::load_settings`/`save_setting`; garbage falls
  back per-field). All three are injected into the shell as
  `window.poundSettings` before first paint (`shell_html`), so the app
  never flashes the wrong theme. The head script resolves Auto via
  `matchMedia` and the page themes via `<html data-theme="<palette>">`
  + `color-scheme` (scrollbars/controls follow); a `matchMedia` listener
  follows live system changes while on Auto. Select changes apply
  locally and post `setting\n<name> <value>` (name: theme/light/dark) —
  the host only validates and persists (no push round-trip).
- No white flash on dark themes: the window (Win32 erase brush) AND the
  WebView2 surface (`DefaultBackgroundColor`) are both created in the
  resolved palette's `--bg` (`Theme::resolved_background` in `theme.rs`,
  with Auto resolved Rust-side via `register::system_prefers_dark` —
  the AppsUseLightTheme registry value). The values are pinned to the
  CSS by test; the shell still corrects Auto live via `matchMedia`.
  A settings-page theme/palette change sets the `theme_changed` flag;
  the event loop then live-updates both surfaces
  (`window.set_background_color` + `webview.set_background_color`), so
  closing never flashes the theme the app STARTED in.
- Fonts: the Settings page's **Font** group picks one typeface for
  every reading/editing surface (rendered prose, source pane, code)
  from the installed system families, enumerated once via GDI
  (`fonts.rs`, injected as the flat `window.poundFonts` array). The
  shell only sets the `--reader-choice`/`--mono-choice` variables; the
  fallback stacks live once in `:root`, which composes them into
  `--reader-font`/`--mono-font`. The **Size** row is a slider (10–28 px,
  0.5 steps; the range lives in `theme.rs` `MIN`/`MAX_FONT_SIZE`,
  default 15.5) setting one px size for both panes (`--font-size`). The
  typeface persists as `Font` (`""` = default; an absent value falls
  back to v0.10.0's `ReaderFont`/`MonoFont`), the size as `FontSize`;
  a typeface that is no longer installed falls back to the default —
  dropped at load by the view, refused at save by the ipc handler. A
  size outside the slider's range is refused too — a 0 size once hid
  every glyph (v0.11.0), so the shell also ignores non-positive sizes.
- Window size: remembered across launches — saved to
  `WindowWidth`/`WindowHeight` (physical px) on every resize except
  while maximized, restored at startup when sane (minimums mirror the
  builder's), default 1120×760 logical otherwise
  (`register::window_size` + the `Resized` handler in `view.rs`).
- Closing with unsaved changes: `CloseRequested` shows the shell's
  dialog via `pound.setClosePrompt(true)` instead of exiting. Its buttons
  post `save-and-exit` (the host saves first and only exits when the
  buffer is clean — a failed save keeps the app open with the banner) or
  `exit`; Cancel posts `cancel-exit` to clear a pending update. Both
  answers resolve through `update::plan_exit`: only the
  update flow (the toast's Update button armed the pending flag) may
  run the installer on the way out; a plain close just saves or
  discards and exits — an unconditional `begin_update` here once
  turned every close into a reinstall + relaunch. The ipc handlers
  set an `exit` flag (they run inside WebView2 message dispatch
  and cannot touch `ControlFlow`) which `MainEventsCleared` honors.
- Right-click: WebView2's default context menu (Chromium's Copy / Print /
  empty "More tools" submenu) is disabled via
  `with_default_context_menus(false)`. In the source editor the shell's
  menu offers Cut / Copy / Paste / Select All (Cut/Copy disabled without
  a selection); anywhere else it stays Copy-only for selections.
  Cut/insert go through `execCommand` so they stay on the editor's
  native undo stack and fire the normal input→edit round-trip. Paste is
  host-mediated — WebView2 cannot read the clipboard from JS without a
  permission prompt — via `clipboard-read` ipc and a Win32
  `CF_UNICODETEXT` read (`clipboard.rs`), answered with
  `pound.setClipboardText`, which inserts at the caret like typing.
- **ammonia sanitizes all HTML** — markdown files can embed raw HTML and
  file content must never execute (scripts/handlers/styling stripped).
  `poundimg`/`data` URL schemes must be in ammonia's allowlist or image
  srcs silently vanish.
- Shell HTML/CSS/JS lives in `src/shell.html`, embedded via `include_str!`
  (`SHELL_HTML` in `view.rs`) — a real file, so editors and formatters
  treat it as HTML. To preview/verify it in a browser: `cargo test
  dump_rendered_sample -- --ignored`, then compose `/tmp/pound-preview.html`
  from the shell file + the dumped content (see git history for the recipe)
  and open it with agent-browser. Exported pages can be checked directly:
  `cargo test dump_exported_sample -- --ignored` writes
  `/tmp/pound-export-sample.html`.


## Commands

```bash
cargo test                                      # unit tests (headless)
cargo fmt --all && cargo clippy --all-targets   # keep CI green
cargo check --target x86_64-pc-windows-msvc     # type-check Windows code
./scripts/package.sh                            # build dist/pound-<ver>-win64.zip (MSVC host)
powershell -ExecutionPolicy Bypass -File tools/install.ps1 -SetDefault  # on Windows
```

## Release process

1. Bump `version` in `Cargo.toml` (package.sh derives the zip name from it —
   keep the git tag identical).
2. Commit, then tag and push:
   ```bash
   git tag v0.X.Y && git push origin main --tags
   ```
3. `.github/workflows/release.yml` runs the tests, builds the zip on a
   windows-latest MSVC runner via `scripts/package.sh` (CRT and
   WebView2Loader statically linked; an import-table check refuses to ship
   exes needing non-system DLLs) and publishes a GitHub Release with notes
   generated from the commits since the previous tag. Users install with the
   one-liner in the README (the installer fetches `pound-*-win64.zip` from
   the latest release via the GitHub API).
4. Rewrite the release notes user-facing — the auto-generated commit list
   is only a placeholder:
   ```bash
   gh release edit vX.Y.Z --notes-file <file>
   ```
   House style (see v0.1.0/v0.1.1/v0.2.0): `# vX.Y.Z` header, one-line
   summary, `**Bold lead** — description` bullets covering user-visible
   changes only, wrapped ~76 columns. Editing later is safe — release.yml
   sets notes at creation time and never touches an existing body.

Everyday pushes to main are preservation-only (no CI). Pull requests and
manual dispatch run `.github/workflows/ci.yml` (fmt, clippy, tests,
Windows cross-check).

## In-app updates

- On start the view spawns a thread that asks the GitHub API for the latest
  release tag (`src/update.rs` — one PowerShell `Invoke-RestMethod`, no
  HTTP-client dependency; silent on failure) and shows a bottom-right toast
  when newer. `is_newer` refuses anything unparseable (incl. prerelease
  suffixes) so it can never nag about a bogus version.
- The toast's **Update** posts `update` over ipc. In-place
  upgrade is possible exactly because the official installer already is
  one: the app spawns `tools/install.ps1 -Relaunch -OpenPath <file>`
  (CREATE_NO_WINDOW, stdout piped) and STAYS ALIVE showing progress — the
  installer's own Write-Step lines stream into the toast, and its
  graceful close closes the app at the replace step (a failed spawn
  falls back to a plain close/exit; with unsaved edits the close prompt
  runs first — Save or Don't save proceed to the installer, Cancel posts
  `cancel-exit` and clears the pending update). `-OpenPath` reopens the
  previously open file after the relaunch.
- GOTCHA: both the check and the installer are silent PowerShell spawns;
  a failed upgrade just leaves the old version running. While updating,
  the editor is locked and CloseRequested exits without prompting (the
  buffer was already saved or discarded).

## Windows integration notes (context for `register.rs`)

- Everything is HKCU-only: ProgIds `Pound.md` + `Pound.table`, and for
  every openable extension (`.md`, `.csv`, `.tsv`) an `OpenWithProgids`
  entry, an `Explorer\FileExts\<ext>\OpenWithProgids` entry,
  `RegisteredApplications` + `Software\Pound\Capabilities` (this is
  what puts us in the system app list and the per-extension "Open with"
  lists), then `SHChangeNotify`.
- `--default` sets the per-user `.md` class default (only `.md`: a flag
  that also claimed the data formats would decide a preference the user
  never expressed). Explorer's `UserChoice`
  is hash-protected by Windows — no third-party app may set it silently;
  if the "choose an app" dialog still appears, the user picks Pound once
  with "Always". Platform restriction, not a bug.
- `install.ps1` copies the exe to `%LOCALAPPDATA%\Pound`, adds a Start-menu
  shortcut + user PATH, and calls `pound register`. The same command
  upgrades: it closes a running Pound first (Windows locks a running exe)
  and re-registers idempotently.
- The binary is built with the GUI subsystem (`windows_subsystem = "windows"`);
  CLI subcommands attach to the parent console for output (unless stdout is
  already a console/pipe/file, so redirection keeps working).
- GOTCHA: PowerShell's call operator does NOT wait for GUI-subsystem
  binaries and leaves `$LASTEXITCODE` unset. Launch pound from scripts with
  `Start-Process -Wait -PassThru` (see tools/install.ps1 for the pattern,
  including output capture for diagnostics).
- GOTCHA: profiles with dots in the username can get 8.3 short-form env
  dirs (`C:\Users\HENDRY~1.CHO\...` in TEMP/LOCALAPPDATA), which trip
  Remove-Item in Windows PowerShell 5.1 with a *terminating* error that
  `-ErrorAction SilentlyContinue` cannot suppress. Canonicalize via
  `Resolve-Path`/`GetFolderPath`, use `-LiteralPath`, and guard cleanup
  with try/catch.

## Debugging

- `pound --help` / `pound --version` print to the console (AttachConsole).
- Windows-only test trap: `Path::canonicalize` returns `\\?\`-prefixed
  verbatim paths on Windows but plain paths on Linux, so comparisons
  against raw `canonicalize()` output pass locally and fail on Windows
  CI. Compare with `model::strip_verbatim_prefix(...)` instead (this cost
  the first v0.2.0 tag its release run).
- The GUI runs on Windows only; on Linux, non-Windows hosts print a notice.
  Verify the shell/rendering in any browser via the `dump_rendered_sample`
  recipe in Rendering notes.
- Debug builds open WebView2 devtools automatically (`with_devtools` in
  `view.rs`) — right-click → Inspect in the app.
- Registry state to inspect when file association misbehaves:
  `HKCU\Software\Classes\.md`, `HKCU\Software\Classes\Pound.md`,
  `HKCU\Software\RegisteredApplications`,
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.md`.
