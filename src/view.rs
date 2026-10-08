//! View layer: egui/eframe rendering.
//!
//! The view is intentionally "dumb": it renders the model's state and
//! forwards user intents (open file, toggle source, click link…) to the
//! presenter. No document logic lives here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{
    text::{LayoutJob, TextFormat},
    CentralPanel, Color32, ColorImage, Frame, Grid, Image, Label, Layout, Margin, RichText,
    ScrollArea, SidePanel, Stroke, TextureHandle, TextureOptions, TopBottomPanel, Ui,
};

use crate::markdown::{Block, Inline, Style};
use crate::model::Document;
use crate::presenter::Presenter;

/// How often to check the document for on-disk changes.
const POLL_INTERVAL: Duration = Duration::from_millis(800);

/// Font size per heading level (H1..H6).
const HEADING_SIZES: [f32; 6] = [28.0, 22.0, 19.0, 17.0, 15.0, 14.0];

/// Custom font family holding the bold weight. egui's bundled fonts only
/// include Ubuntu-Light, so bold runs select this family instead.
const BOLD_FAMILY: &str = "pound-bold";
/// Base text style for a group of runs (heading size, quote dimming…).
#[derive(Clone, Copy)]
struct TextBase {
    size: Option<f32>,
    strong: bool,
    weak: bool,
}

const BODY: TextBase = TextBase {
    size: None,
    strong: false,
    weak: false,
};

/// Mutable state shared by the render helpers for one frame.
struct RenderCtx<'a> {
    doc_dir: &'a Path,
    images: &'a mut HashMap<String, TextureHandle>,
    clicked_links: Vec<String>,
    table_counter: u32,
    list_counter: u32,
}

impl<'a> RenderCtx<'a> {
    fn push_link(&mut self, url: &str) {
        self.clicked_links.push(url.to_owned());
    }
}

pub struct AppView {
    presenter: Presenter,
    /// Decoded image textures, keyed by resolved file path.
    images: HashMap<String, TextureHandle>,
    /// Model revision the texture cache belongs to.
    cached_revision: u64,
    last_title: String,
    last_poll: Instant,
}

impl AppView {
    pub fn new(cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> AppView {
        install_fonts(&cc.egui_ctx);
        AppView {
            presenter: Presenter::new(file.as_deref()),
            images: HashMap::new(),
            cached_revision: 0,
            last_title: "Pound".to_owned(),
            last_poll: Instant::now(),
        }
    }
}

/// Install the reading fonts. On Windows we prefer the system sans-serif
/// — **Segoe UI** (the same font VSCode's markdown preview uses on
/// Windows), with its true bold weight, plus Consolas for code. These are
/// loaded from the OS at runtime and never redistributed with the app.
/// Elsewhere (Linux dev machines) we fall back to egui's defaults plus a
/// bundled Ubuntu-Bold so real bold still renders.
fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

/// Load a font that ships with Windows (e.g. `segoeui.ttf`) from the system
/// fonts directory. Returns `None` when unavailable (non-Windows or odd
/// installs) so callers can fall back.
fn load_system_font(name: &str) -> Option<Vec<u8>> {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_owned());
    std::fs::read(format!(r"{windir}\Fonts\{name}")).ok()
}

fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();

    match (
        load_system_font("segoeui.ttf"),
        load_system_font("segoeuib.ttf"),
    ) {
        (Some(regular), Some(bold)) => {
            fonts.font_data.insert(
                "segoe-ui".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(regular)),
            );
            fonts.font_data.insert(
                "segoe-ui-bold".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bold)),
            );
            fonts.families.insert(
                egui::FontFamily::Proportional,
                vec![
                    "segoe-ui".to_owned(),
                    "NotoEmoji-Regular".to_owned(),
                    "emoji-icon-font".to_owned(),
                ],
            );
            fonts.families.insert(
                egui::FontFamily::Name(BOLD_FAMILY.into()),
                vec![
                    "segoe-ui-bold".to_owned(),
                    "segoe-ui".to_owned(), // glyph fallback
                    "NotoEmoji-Regular".to_owned(),
                    "emoji-icon-font".to_owned(),
                ],
            );
            if let Some(consolas) = load_system_font("consola.ttf") {
                fonts.font_data.insert(
                    "consolas".to_owned(),
                    std::sync::Arc::new(egui::FontData::from_owned(consolas)),
                );
                if let Some(mono) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                    mono.insert(0, "consolas".to_owned());
                }
            }
        }
        _ => {
            // Fallback: egui's default Ubuntu-Light plus the bundled
            // Ubuntu-Bold (same typeface + license) for real bold.
            fonts.font_data.insert(
                "pound-ubuntu-bold".to_owned(),
                std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                    "../assets/fonts/Ubuntu-Bold.ttf"
                ))),
            );
            fonts.families.insert(
                egui::FontFamily::Name(BOLD_FAMILY.into()),
                vec![
                    "pound-ubuntu-bold".to_owned(),
                    "Ubuntu-Light".to_owned(), // glyph fallback
                    "NotoEmoji-Regular".to_owned(),
                    "emoji-icon-font".to_owned(),
                ],
            );
        }
    }
    fonts
}

impl eframe::App for AppView {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // --- input: drag & drop + external file changes --------------------
        // Ctrl+U toggles the source pane (like browser "view source").
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::U)) {
            self.presenter.toggle_source();
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|d| d.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.presenter.open_dropped(&dropped);
        }
        if self.last_poll.elapsed() >= POLL_INTERVAL {
            self.last_poll = Instant::now();
            self.presenter.reload_if_changed();
        }
        ctx.request_repaint_after(POLL_INTERVAL);

        // Invalidate the image cache when a new document revision appears.
        if self.presenter.model.revision != self.cached_revision {
            self.cached_revision = self.presenter.model.revision;
            self.images.clear();
        }

        // Keep the window title in sync with the document.
        let want_title = match &self.presenter.model.document {
            Some(doc) => format!("{} — Pound", doc.name()),
            None => "Pound".to_owned(),
        };
        if want_title != self.last_title {
            self.last_title = want_title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(want_title));
        }

        // Split borrows so the render helpers can use the document and the
        // texture cache at the same time.
        let AppView {
            presenter, images, ..
        } = self;

        TopBottomPanel::top("top_bar").show(ctx, |ui| top_bar(ui, presenter));

        if presenter.model.show_source {
            let width = ctx.screen_rect().width() * 0.42;
            SidePanel::right("source_panel")
                .resizable(true)
                .default_width(width)
                .show(ctx, |ui| source_panel(ui, &presenter.model));
        }

        CentralPanel::default().show(ctx, |ui| {
            Frame::default()
                .inner_margin(Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(error) = presenter.model.error.clone() {
                                error_banner(ui, presenter, &error);
                            }
                            if presenter.model.document.is_some() {
                                let doc = presenter.model.document.as_ref().expect("checked above");
                                let dir = doc.dir();
                                let mut rc = RenderCtx {
                                    doc_dir: &dir,
                                    images,
                                    clicked_links: Vec::new(),
                                    table_counter: 0,
                                    list_counter: 0,
                                };
                                render_blocks(ui, doc, &doc.blocks, BODY, &mut rc);
                                for url in rc.clicked_links {
                                    presenter.open_link(&url);
                                }
                            } else {
                                welcome(ui, presenter);
                            }
                        });
                });
        });
    }
}

// ---------------------------------------------------------------------------
// chrome (top bar, source panel, banners, welcome screen)
// ---------------------------------------------------------------------------

fn top_bar(ui: &mut Ui, presenter: &mut Presenter) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.strong("Pound");
        ui.separator();

        #[cfg(windows)]
        if ui.button("Open…").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Markdown", &["md", "markdown"])
                .pick_file()
            {
                presenter.open_path(&path);
            }
        }

        if presenter.model.document.is_some() && ui.button("Reload").clicked() {
            presenter.reload();
        }

        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            let mut show_source = presenter.model.show_source;
            if ui.checkbox(&mut show_source, "Source").changed() {
                presenter.set_show_source(show_source);
            }
            if let Some(doc) = &presenter.model.document {
                ui.weak(doc.path.display().to_string())
                    .on_hover_text("Full path of the open document");
            }
        });
    });
    ui.add_space(3.0);
}

fn source_panel(ui: &mut Ui, model: &crate::model::Model) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.strong("Source");
        if let Some(doc) = &model.document {
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(doc.path.display().to_string());
            });
        }
    });
    ui.separator();
    if let Some(doc) = &model.document {
        ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.add(Label::new(RichText::new(&doc.source).monospace()).selectable(true));
            });
    } else {
        ui.weak("No document open.");
    }
}

fn error_banner(ui: &mut Ui, presenter: &mut Presenter, error: &str) {
    Frame::default()
        .fill(ui.visuals().error_fg_color.gamma_multiply(0.12))
        .inner_margin(Margin::same(8))
        .corner_radius(4)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, "⚠");
                ui.colored_label(ui.visuals().error_fg_color, error);
                if ui.small_button("Dismiss").clicked() {
                    presenter.dismiss_error();
                }
            });
        });
    ui.add_space(6.0);
}

fn welcome(ui: &mut Ui, presenter: &mut Presenter) {
    let _ = presenter; // used by the Windows-only file dialog below
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.28);
        ui.heading("Pound");
        ui.label("A tiny markdown reader.");
        ui.add_space(12.0);

        #[cfg(windows)]
        if ui.button("Open a markdown file…").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Markdown", &["md", "markdown"])
                .pick_file()
            {
                presenter.open_path(&path);
            }
        }

        ui.add_space(8.0);
        ui.weak("…or drop a .md file anywhere in this window.");
        ui.weak("From a terminal:  pound path/to/file.md");
        #[cfg(windows)]
        ui.weak("Register as the .md app:  pound register --default");
    });
}

// ---------------------------------------------------------------------------
// document rendering
// ---------------------------------------------------------------------------

fn render_blocks(
    ui: &mut Ui,
    doc: &Document,
    blocks: &[Block],
    base: TextBase,
    rc: &mut RenderCtx<'_>,
) {
    for block in blocks {
        match block {
            Block::Heading { level, inlines } => {
                let size = HEADING_SIZES[(level.saturating_sub(1) as usize).min(5)];
                let heading_base = TextBase {
                    size: Some(size),
                    strong: true,
                    weak: base.weak,
                };
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| render_inlines(ui, inlines, heading_base, rc));
                ui.add_space(4.0);
            }
            Block::Paragraph(inlines) => {
                ui.horizontal_wrapped(|ui| render_inlines(ui, inlines, base, rc));
                ui.add_space(8.0);
            }
            Block::Code { lang, text } => render_code(ui, lang.as_deref(), text, false),
            Block::Html(text) => render_code(ui, Some("html"), text, true),
            Block::Quote(blocks) => {
                let fill = ui.visuals().faint_bg_color;
                Frame::default()
                    .fill(fill)
                    .inner_margin(Margin {
                        left: 12,
                        right: 8,
                        top: 6,
                        bottom: 6,
                    })
                    .corner_radius(4)
                    .show(ui, |ui| {
                        let quote_base = TextBase { weak: true, ..base };
                        render_blocks(ui, doc, blocks, quote_base, rc);
                    });
                ui.add_space(6.0);
            }
            Block::List { start, items } => render_list(ui, doc, items, *start, base, rc),
            Block::Rule => {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
            }
            Block::Table { head, rows } => render_table(ui, head, rows, base, rc),
        }
    }
}

fn render_list(
    ui: &mut Ui,
    doc: &Document,
    items: &[crate::markdown::Item],
    start: Option<u64>,
    base: TextBase,
    rc: &mut RenderCtx<'_>,
) {
    let mut number = start.unwrap_or(1);
    for item in items {
        let marker = match (item.task, start) {
            (Some(true), _) => "☑".to_owned(),
            (Some(false), _) => "☐".to_owned(),
            (None, Some(_)) => format!("{number}. "),
            (None, None) => "•  ".to_owned(),
        };
        // List markers are regular weight (like most markdown renderers).
        let marker_label = marker;

        // Common case: a single-paragraph item renders inline with its marker.
        let inline = item.blocks.len() == 1 && matches!(item.blocks[0], Block::Paragraph(_));

        if inline {
            let Block::Paragraph(inlines) = &item.blocks[0] else {
                unreachable!()
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(marker_label);
                render_inlines(ui, inlines, base, rc);
            });
        } else {
            ui.label(marker_label);
            let id = egui::Id::new("list-item").with(rc.list_counter);
            rc.list_counter += 1;
            ui.indent(id, |ui| render_blocks(ui, doc, &item.blocks, base, rc));
        }
        if start.is_some() {
            number += 1;
        }
    }
}

fn render_table(
    ui: &mut Ui,
    head: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    base: TextBase,
    rc: &mut RenderCtx<'_>,
) {
    let columns = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return;
    }
    let id = format!("table-{}", rc.table_counter);
    rc.table_counter += 1;
    let head_base = TextBase {
        strong: true,
        ..base
    };
    Grid::new(id)
        .num_columns(columns)
        .striped(true)
        .spacing([16.0, 6.0])
        .min_col_width(48.0)
        .show(ui, |ui| {
            if !head.is_empty() {
                for cell in head {
                    ui.horizontal_wrapped(|ui| render_inlines(ui, cell, head_base, rc));
                }
                ui.end_row();
            }
            for row in rows {
                for cell in row {
                    ui.horizontal_wrapped(|ui| render_inlines(ui, cell, base, rc));
                }
                ui.end_row();
            }
        });
    ui.add_space(8.0);
}

fn render_code(ui: &mut Ui, lang: Option<&str>, text: &str, dim: bool) {
    let bg = if dim {
        ui.visuals().extreme_bg_color
    } else {
        ui.visuals().code_bg_color
    };
    Frame::default()
        .fill(bg)
        .inner_margin(Margin::same(8))
        .corner_radius(4)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if lang.is_some() || !text.is_empty() {
                ui.horizontal(|ui| {
                    if let Some(lang) = lang {
                        ui.label(RichText::new(lang).small().weak().monospace());
                    }
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(text.to_owned());
                        }
                    });
                });
            }
            let mut rich = RichText::new(text).monospace();
            if dim {
                rich = rich.weak();
            }
            ui.add(Label::new(rich).selectable(true));
        });
    ui.add_space(6.0);
}

fn render_inlines(ui: &mut Ui, inlines: &[Inline], base: TextBase, rc: &mut RenderCtx<'_>) {
    // Runs are laid out in a single rich-text galley per contiguous segment
    // (split only around images, which cannot live inside a galley). This
    // gives proper word spacing, line height, and inline link hit-testing.
    let mut job = make_job(ui);
    let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Run { text, style } => {
                // Link spans are tracked in CHARACTER offsets, which is what
                // `CCursor.index` uses for galley hit-testing.
                let start_chars = job.text.chars().count();
                job.append(text.as_str(), 0.0, text_format(ui, base, style));
                if let Some(url) = &style.link {
                    links.push((start_chars..job.text.chars().count(), url.clone()));
                }
            }
            Inline::Image { alt, url } => {
                render_text_job(ui, std::mem::take(&mut job), std::mem::take(&mut links), rc);
                render_image(ui, alt, url, rc);
            }
        }
    }
    render_text_job(ui, job, links, rc);
}

fn make_job(ui: &Ui) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    job
}

/// Lay out one text segment, paint it, and open any clicked link span.
fn render_text_job(
    ui: &mut Ui,
    job: LayoutJob,
    links: Vec<(std::ops::Range<usize>, String)>,
    rc: &mut RenderCtx<'_>,
) {
    if job.text.is_empty() {
        return;
    }
    let galley = ui.ctx().fonts(|fonts| fonts.layout_job(job));
    let (rect, response) = ui.allocate_exact_size(galley.size(), egui::Sense::click());
    ui.painter()
        .galley(rect.min, galley.clone(), ui.visuals().text_color());
    if response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let char_index = galley.cursor_from_pos(pointer - rect.min).index;
            if let Some((_, url)) = links.iter().find(|(range, _)| range.contains(&char_index)) {
                rc.push_link(url);
            }
        }
    }
    if !links.is_empty() {
        // Pointing hand over paragraphs that contain links.
        response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
}

fn text_format(ui: &Ui, base: TextBase, style: &Style) -> TextFormat {
    let size = base
        .size
        .unwrap_or_else(|| ui.text_style_height(&egui::TextStyle::Body));
    let bold = base.strong || style.bold;
    let family = if style.code {
        egui::FontFamily::Monospace
    } else if bold {
        egui::FontFamily::Name(BOLD_FAMILY.into())
    } else {
        egui::FontFamily::Proportional
    };
    let visuals = ui.visuals();
    let color = if base.weak {
        visuals.weak_text_color()
    } else {
        visuals.text_color()
    };
    // Roomier line height for body text (headings stay tighter), similar to
    // typical web markdown rendering.
    let line_height = if base.size.is_some() {
        size * 1.3
    } else {
        size * 1.5
    };
    let mut format = TextFormat {
        font_id: egui::FontId::new(size, family),
        line_height: Some(line_height),
        color,
        background: if style.code {
            visuals.code_bg_color
        } else {
            Color32::TRANSPARENT
        },
        italics: style.italic,
        ..Default::default()
    };
    if style.strike {
        format.strikethrough = Stroke::new(1.0_f32, color);
    }
    if style.underline {
        format.underline = Stroke::new(1.0_f32, color);
    }
    if style.link.is_some() {
        format.color = visuals.hyperlink_color;
        format.underline = Stroke::new(1.0_f32, visuals.hyperlink_color);
    }
    format
}

/// Render an image reference. Only local files are supported for now;
/// remote URLs show a placeholder chip.
fn render_image(ui: &mut Ui, alt: &str, url: &str, rc: &mut RenderCtx<'_>) {
    let remote = {
        let lower = url.to_ascii_lowercase();
        lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("data:")
    };

    if remote {
        image_placeholder(ui, alt, url);
        return;
    }

    let path = rc.doc_dir.join(url);
    let key = path.display().to_string();

    if !rc.images.contains_key(&key) && path.is_file() {
        if let Ok(decoded) = image::open(&path) {
            let rgba = decoded.to_rgba8();
            let (w, h) = rgba.dimensions();
            let color_image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            let texture = ui
                .ctx()
                .load_texture(&key, color_image, TextureOptions::default());
            rc.images.insert(key.clone(), texture);
        }
    }

    if let Some(texture) = rc.images.get(&key) {
        ui.add(Image::from_texture(texture).max_width(560.0));
    } else {
        image_placeholder(ui, alt, url);
    }
}

fn image_placeholder(ui: &mut Ui, alt: &str, url: &str) {
    let text = if alt.is_empty() { "image" } else { alt };
    let chip = RichText::new(format!("🖼 {text}"))
        .small()
        .monospace()
        .background_color(ui.visuals().code_bg_color);
    ui.label(chip).on_hover_text(url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_system_font_returns_none() {
        assert!(load_system_font("definitely-not-a-real-font.ttf").is_none());
    }

    /// Headless smoke test: whichever path `font_definitions` takes (Segoe
    /// UI on Windows, bundled Ubuntu elsewhere), the bold family parses
    /// through egui's font stack and lays out to a non-empty galley.
    #[test]
    fn bold_font_lays_out() {
        let fonts = egui::text::Fonts::new(
            1.0,
            8192,
            egui::epaint::AlphaFromCoverage::LIGHT_MODE_DEFAULT,
            font_definitions(),
        );

        let mut job = LayoutJob::default();
        job.append(
            "bold text",
            0.0,
            TextFormat {
                font_id: egui::FontId::new(14.0, egui::FontFamily::Name(BOLD_FAMILY.into())),
                ..Default::default()
            },
        );
        let galley = fonts.layout_job(job);
        assert!(galley.size().x > 0.0, "bold galley should have width");
        assert!(galley.size().y > 0.0, "bold galley should have height");
    }
}
