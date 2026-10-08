# AGENTS.md — Pound playbook

Environment, build, release and debugging notes for humans *and* AI coding
agents. (Companion repo to [linkport](https://github.com/butageek/linkport);
same conventions.)

## What this is

Pound is a markdown reader for Windows written in Rust (eframe/egui +
pulldown-cmark). It renders `.md` files by default and can show the raw
source side by side (Ctrl+U). Architecture is strict MVP
(Model–View–Presenter) — keep it that way:

| Layer | File | Rule |
|---|---|---|
| Model | `src/model.rs` | state only, no UI types |
| Parsing | `src/markdown.rs` | pulldown-cmark events → neutral `Block`/`Inline` tree, unit-tested headlessly |
| Presenter | `src/presenter.rs` | intents/use-cases, owns the Model, no egui types |
| View | `src/view.rs` | egui rendering only; forwards events to the presenter |
| Windows glue | `src/register.rs` | HKCU registry: app-list registration + `.md` association |

## Environment

- Development happens on Linux/WSL; the shipped binary is Windows.
- Windows type-checking needs the target: `rustup target add x86_64-pc-windows-msvc`
  (or `x86_64-pc-windows-gnu` for full cross-builds — needs `mingw-w64` for
  linking *and* the icon's windres).
- The exe icon (`assets/pound.ico`) is regenerated with `python3 tools/gen_icon.py`
  (stdlib only, no PIL).

## Rendering notes

- **Fonts**: on Windows the app loads the system sans-serif — Segoe UI
  (regular + true bold) and Consolas for code — straight from
  `%WINDIR%\Fonts` at runtime (`load_system_font` in `view.rs`); Segoe UI
  is licensed to the OS, so it must never be committed to the repo. On
  non-Windows machines it falls back to egui's Ubuntu-Light plus the
  bundled `assets/fonts/Ubuntu-Bold.ttf`.
- egui's bundled fonts have **no bold weight** and `RichText::strong()` only
  strengthens the COLOR. Real bold comes from the `pound-bold` font family
  (`install_fonts`/`font_definitions` in `view.rs`), selected via `FontId`
  in `text_format`.
- Inline text renders as one `LayoutJob` galley per paragraph (word spacing,
  `line_height`, inline code backgrounds, underline/strike); links are
  hit-tested via `cursor_from_pos(...).index` — a CHARACTER offset, so link
  spans are tracked in chars, not bytes.
- Code chips: monospace fonts have much shorter ascents than Segoe UI, so
  code runs use the font's natural line height (no shared row pitch) plus
  centered valign — otherwise the chip's baseline floats noticeably high.
- Tables avoid `egui::Grid` on purpose: Grid measures cells with a tiny
  available width, which collapses pre-wrapped galleys into
  one-character-per-line and gigantic rows. `render_table` measures each
  cell's natural single-line width, fits columns into the available width
  (HTML width:100% style), and lays out rows manually.
- `markdown.rs` maps common inline-HTML formatting tags onto style flags
  (`inline_html_tag`); unknown tags are ignored, `<br>` is a hard break.

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
- `pound file.md` on a headless Linux box fails in winit with
  "neither WAYLAND_DISPLAY nor DISPLAY is set" — that is expected; use a
  Windows machine or X server for GUI testing.
- Registry state to inspect when file association misbehaves:
  `HKCU\Software\Classes\.md`, `HKCU\Software\Classes\Pound.md`,
  `HKCU\Software\RegisteredApplications`,
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.md`.
