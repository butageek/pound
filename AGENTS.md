# AGENTS.md — Pound playbook

Environment, build, release and debugging notes for humans *and* AI coding
agents. (Companion repo to [linkport](https://github.com/butageek/linkport);
same conventions.)

## What this is

Pound is a lightweight file viewer for Windows written in Rust (tao +
wry + pulldown-cmark). It started as a markdown reader — `.md` files
render by default with the raw source side by side (Ctrl+U), and the
source pane is a live editor: typing re-renders the preview and `Ctrl+S`
saves back to disk. It is growing into a viewer for more commonly used
file types (CSV next). Architecture is strict MVP
(Model–View–Presenter) — keep it that way:

| Layer | File | Rule |
|---|---|---|
| Model | `src/model.rs` | state only, no UI types |
| Rendering input | `src/markdown.rs` | pulldown-cmark → sanitized HTML, unit-tested headlessly |
| Presenter | `src/presenter.rs` | intents/use-cases, owns the Model, no GUI types |
| View | `src/view.rs` | WebView2 shell (tao + wry): shell HTML/CSS/JS, JS bridge; forwards intents to the presenter |
| Windows glue | `src/register.rs` | HKCU registry: app-list registration + `.md` association |

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
- Rust → JS: `push_document` evaluates `pound.setContent(html, source,
  title, path, status)` + `pound.setError(msg)` — strings JSON-escaped by
  `json_str` (quoting only; XSS is handled by ammonia). `status` feeds the
  bottom status bar (path + file type + char/line counts; `model.rs` strips
  the `\\?\` verbatim prefix `canonicalize` adds, so users see `C:\…`).
- JS → Rust: in-page controls navigate to `pound://dismiss-error` (error
  banner), intercepted by the navigation handler which calls presenter
  intents. There is deliberately no Open/Reload UI: files arrive by
  double-click, drag-and-drop or CLI, and on-disk edits auto-reload.
  http(s)/mailto links are opened externally and never navigate the reader.
- Startup handshake: `evaluate_script` right after `build` races
  WebView2's `NavigateToString` commit — a warm-started second instance
  regularly loses (the script runs in the still-blank document where
  `pound` is undefined; wry ignores the exception), which showed up as
  "opened a file into a new window, no content". The shell posts
  `window.ipc.postMessage('ready')` when its scripts parsed and the view
  re-pushes the document. `window.ipc` is injected only when
  `with_ipc_handler` is registered, so it's a no-op in plain browsers and
  the preview recipe below keeps working.
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
- Disk-conflict policy: unsaved edits pause auto-reload. The status bar
  shows `edited`, plus `changed on disk` when the file was modified
  externally (saving resolves it, last writer wins); a same-content mtime
  touch never reloads. The window title prefixes `*` while unsaved.
- Themes: a **Settings** page (top-bar button) with the Zed-style model:
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
- Closing with unsaved changes: `CloseRequested` shows the shell's
  dialog via `pound.setClosePrompt(true)` instead of exiting. Its buttons
  post `save-and-exit` (the host saves first and only exits when the
  buffer is clean — a failed save keeps the app open with the banner) or
  `exit`; Cancel posts `cancel-exit` to clear a pending update. The ipc
  handlers set an `exit` flag (they run inside WebView2 message dispatch
  and cannot touch `ControlFlow`) which `MainEventsCleared` honors.
- Right-click: WebView2's default context menu (Chromium's Copy / Print /
  empty "More tools" submenu) is disabled via
  `with_default_context_menus(false)`; the shell shows a minimal Copy-only
  menu when text is selected instead.
- **ammonia sanitizes all HTML** — markdown files can embed raw HTML and
  file content must never execute (scripts/handlers/styling stripped).
  `poundimg`/`data` URL schemes must be in ammonia's allowlist or image
  srcs silently vanish.
- Shell HTML/CSS/JS lives in `SHELL_HTML` in `view.rs`. To preview/verify it
  in a browser: `cargo test dump_rendered_sample -- --ignored`, then compose
  `/tmp/pound-preview.html` from the shell const + the dumped content (see
  git history for the recipe) and open it with agent-browser.


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

- Everything is HKCU-only: ProgId `Pound.md`, `.md\OpenWithProgids`,
  `Explorer\FileExts\.md\OpenWithProgids`, `RegisteredApplications` +
  `Software\Pound\Capabilities` (this is what puts us in the system app
  list), then `SHChangeNotify`.
- `--default` sets the per-user `.md` class default. Explorer's `UserChoice`
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
