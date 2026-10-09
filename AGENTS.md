# AGENTS.md — Pound playbook

Environment, build, release and debugging notes for humans *and* AI coding
agents. (Companion repo to [linkport](https://github.com/butageek/linkport);
same conventions.)

## What this is

Pound is a lightweight file viewer for Windows written in Rust (tao +
wry + pulldown-cmark). It started as a markdown reader — `.md` files
render by default with the raw source side by side (Ctrl+U) — and is
growing into an editor and a viewer for more commonly used file types
(CSV next). Architecture is strict MVP (Model–View–Presenter) — keep it
that way:

| Layer | File | Rule |
|---|---|---|
| Model | `src/model.rs` | state only, no UI types |
| Rendering input | `src/markdown.rs` | pulldown-cmark → sanitized HTML, unit-tested headlessly |
| Presenter | `src/presenter.rs` | intents/use-cases, owns the Model, no GUI types |
| View | `src/view.rs` | WebView2 shell (tao + wry): shell HTML/CSS/JS, JS bridge; forwards intents to the presenter |
| Windows glue | `src/register.rs` | HKCU registry: app-list registration + `.md` association |

## Environment

- Development happens on Linux/WSL; the shipped binary is Windows. The GUI
  (tao/wry/rfd) is `[target.'cfg(windows)'.dependencies]`-gated, so Linux
  hosts run `cargo test` only and cannot open the GUI.
- Windows type-checking needs the target: `rustup target add x86_64-pc-windows-msvc`
  (a plain `cargo check --target` needs no linker; the icon is only embedded
  in real builds). WebView2 ships with Windows 10/11.
- **Release builds use MSVC** (CI builds on `windows-latest`; locally needs
  VS Build Tools). NEVER build releases with windows-gnu/mingw: mingw cannot
  link Microsoft's static WebView2Loader lib, so the exe imports
  `WebView2Loader.dll` dynamically and dies at startup with
  STATUS_DLL_NOT_FOUND (0xC0000135) on user machines — this shipped as
  v0.2.0 and is why the release runner is MSVC.
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
- Local images: `markdown.rs` rewrites relative srcs to percent-encoded
  `poundimg://<abs path>`; the view serves them from disk via a custom
  protocol. Unknown/missing files keep their src (broken-image marker).
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
./scripts/package.sh                            # cross-build + dist/pound-<ver>-win64.zip
powershell -ExecutionPolicy Bypass -File tools/install.ps1 -SetDefault  # on Windows
```

## Release process

1. Bump `version` in `Cargo.toml` (package.sh derives the zip name from it —
   keep the git tag identical).
2. Commit, then tag and push:
   ```bash
   git tag v0.X.Y && git push origin main --tags
   ```
3. `.github/workflows/release.yml` runs the tests, cross-builds the zip
   via mingw-w64 and publishes a GitHub Release with notes generated from
   the commits since the previous tag. Users install with the one-liner in
   the README (the installer fetches `pound-*-win64.zip` from the latest
   release via the GitHub API).

Everyday pushes to main are preservation-only (no CI). Pull requests and
manual dispatch run `.github/workflows/ci.yml` (fmt, clippy, tests,
Windows cross-check).

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
- The GUI runs on Windows only; on Linux, non-Windows hosts print a notice.
  Verify the shell/rendering in any browser via the `dump_rendered_sample`
  recipe in Rendering notes.
- Debug builds open WebView2 devtools automatically (`with_devtools` in
  `view.rs`) — right-click → Inspect in the app.
- Registry state to inspect when file association misbehaves:
  `HKCU\Software\Classes\.md`, `HKCU\Software\Classes\Pound.md`,
  `HKCU\Software\RegisteredApplications`,
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.md`.
