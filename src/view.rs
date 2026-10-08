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
    CentralPanel, ColorImage, Frame, Grid, Image, Label, Layout, Margin, RichText, ScrollArea,
    SidePanel, TextureHandle, TextureOptions, TopBottomPanel, Ui,
};

use crate::markdown::{Block, Inline, Style};
use crate::model::Document;
use crate::presenter::Presenter;

/// How often to check the document for on-disk changes.
const POLL_INTERVAL: Duration = Duration::from_millis(800);

/// Font size per heading level (H1..H6).
const HEADING_SIZES: [f32; 6] = [28.0, 22.0, 19.0, 17.0, 15.0, 14.0];

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
    pub fn new(_cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> AppView {
        AppView {
            presenter: Presenter::new(file.as_deref()),
            images: HashMap::new(),
            cached_revision: 0,
            last_title: "Pound".to_owned(),
            last_poll: Instant::now(),
        }
    }
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
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| render_inlines(ui, inlines, heading_base, rc));
                ui.add_space(2.0);
            }
            Block::Paragraph(inlines) => {
                ui.horizontal_wrapped(|ui| render_inlines(ui, inlines, base, rc));
                ui.add_space(5.0);
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
        let marker_label = RichText::new(marker).strong();

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
    for inline in inlines {
        match inline {
            Inline::Run { text, style } => render_run(ui, text, style, base, rc),
            Inline::Image { alt, url } => render_image(ui, alt, url, rc),
        }
    }
}

fn render_run(ui: &mut Ui, text: &str, style: &Style, base: TextBase, rc: &mut RenderCtx<'_>) {
    if let Some(url) = &style.link {
        if ui.link(text).clicked() {
            rc.push_link(url);
        }
        return;
    }

    let mut rich = RichText::new(text);
    if let Some(size) = base.size {
        rich = rich.size(size);
    }
    if base.strong || style.bold {
        rich = rich.strong();
    }
    if style.italic {
        rich = rich.italics();
    }
    if style.strike {
        rich = rich.strikethrough();
    }
    if style.code {
        rich = rich
            .monospace()
            .background_color(ui.visuals().code_bg_color);
    }
    if base.weak {
        rich = rich.weak();
    }
    ui.label(rich);
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
