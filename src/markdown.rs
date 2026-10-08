//! Markdown parsing (Model layer).
//!
//! Converts `pulldown-cmark` events into a small, UI-neutral tree of
//! `Block`/`Inline` values. The view layer renders this tree with egui
//! without knowing anything about the markdown parser.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Inline character style captured at the moment a text run was parsed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// When set, this run should be rendered as a hyperlink to this URL.
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Run { text: String, style: Style },
    Image { alt: String, url: String },
}

/// One list item; `task` is `Some(checked)` for `- [x]` style task items.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    Code {
        lang: Option<String>,
        text: String,
    },
    /// Raw HTML found in the document; shown as a dim code block.
    Html(String),
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<Item>,
    },
    Rule,
    Table {
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
}

enum Frame {
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<Item>,
    },
    Item(Vec<Block>),
}

#[derive(Default)]
struct TableBuild {
    head: Vec<Vec<Inline>>,
    rows: Vec<Vec<Vec<Inline>>>,
    row: Vec<Vec<Inline>>,
    cell: Option<Vec<Inline>>,
}

#[derive(Default)]
struct Builder {
    root: Vec<Block>,
    frames: Vec<Frame>,
    inlines: Vec<Inline>,
    style: Style,
    image: Option<(String, String)>,        // (url, alt)
    code: Option<(Option<String>, String)>, // (lang, text)
    html: Option<String>,
    table: Option<TableBuild>,
    task: Option<bool>,
}

impl Builder {
    /// Where the next completed block belongs.
    fn top(&mut self) -> &mut Vec<Block> {
        match self.frames.last_mut() {
            Some(Frame::Quote(blocks)) | Some(Frame::Item(blocks)) => blocks,
            Some(Frame::List { items, .. }) => {
                // A block that appears directly inside a list (no Item) is
                // treated as an implicit single item.
                items.push(Item {
                    task: None,
                    blocks: Vec::new(),
                });
                let Item { blocks, .. } = items.last_mut().expect("just pushed");
                blocks
            }
            None => &mut self.root,
        }
    }

    fn push_block(&mut self, block: Block) {
        self.top().push(block);
    }

    fn flush_inlines(&mut self) {
        if !self.inlines.is_empty() {
            let inlines = std::mem::take(&mut self.inlines);
            self.push_block(Block::Paragraph(inlines));
        }
    }

    fn text(&mut self, text: &str) {
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(text);
        } else if let Some((_, buf)) = &mut self.code {
            buf.push_str(text);
        } else if let Some(html) = &mut self.html {
            html.push_str(text);
        } else if let Some(table) = &mut self.table {
            if let Some(cell) = &mut table.cell {
                cell.push(Inline::Run {
                    text: text.to_owned(),
                    style: self.style.clone(),
                });
            }
        } else {
            self.inlines.push(Inline::Run {
                text: text.to_owned(),
                style: self.style.clone(),
            });
        }
    }

    fn code_span(&mut self, code: &str) {
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(code);
            return;
        }
        let mut style = self.style.clone();
        style.code = true;
        let run = Inline::Run {
            text: code.to_owned(),
            style,
        };
        if let Some(table) = &mut self.table {
            if let Some(cell) = &mut table.cell {
                cell.push(run);
                return;
            }
        }
        self.inlines.push(run);
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => self.end(end),
            Event::Text(t) => self.text(&t),
            Event::Code(c) => self.code_span(&c),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.text("\n"),
            Event::Rule => {
                self.flush_inlines();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(done) => self.task = Some(done),
            Event::Html(h) | Event::InlineHtml(h) => {
                if let Some(html) = &mut self.html {
                    html.push_str(&h);
                } else if let Some((_, buf)) = &mut self.code {
                    buf.push_str(&h);
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { .. } => self.flush_inlines(),
            Tag::BlockQuote(_) => {
                self.flush_inlines();
                self.frames.push(Frame::Quote(Vec::new()));
            }
            Tag::List(start) => {
                self.flush_inlines();
                self.frames.push(Frame::List {
                    start,
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                self.flush_inlines();
                self.frames.push(Frame::Item(Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                self.flush_inlines();
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => Some(l.to_string()),
                    CodeBlockKind::Indented => None,
                };
                self.code = Some((lang, String::new()));
            }
            Tag::Emphasis => self.style.italic = true,
            Tag::Strong => self.style.bold = true,
            Tag::Strikethrough => self.style.strike = true,
            Tag::Link { dest_url, .. } => self.style.link = Some(dest_url.to_string()),
            Tag::Image { dest_url, .. } => self.image = Some((dest_url.to_string(), String::new())),
            Tag::HtmlBlock => self.html = Some(String::new()),
            Tag::Table(_) => {
                self.flush_inlines();
                self.table = Some(TableBuild::default());
            }
            Tag::TableHead => {}
            Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.row = Vec::new();
                }
            }
            Tag::TableCell => {
                if let Some(t) = &mut self.table {
                    t.cell = Some(Vec::new());
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Paragraph => self.flush_inlines(),
            TagEnd::Heading(level) => {
                let inlines = std::mem::take(&mut self.inlines);
                let level = heading_level(level);
                self.push_block(Block::Heading { level, inlines });
            }
            TagEnd::BlockQuote(_) => {
                if matches!(self.frames.last(), Some(Frame::Quote(_))) {
                    if let Some(Frame::Quote(blocks)) = self.frames.pop() {
                        self.push_block(Block::Quote(blocks));
                    }
                }
            }
            TagEnd::List(_) => {
                if matches!(self.frames.last(), Some(Frame::List { .. })) {
                    if let Some(Frame::List { start, items }) = self.frames.pop() {
                        self.push_block(Block::List { start, items });
                    }
                }
            }
            TagEnd::Item => {
                self.flush_inlines();
                if matches!(self.frames.last(), Some(Frame::Item(_))) {
                    if let Some(Frame::Item(blocks)) = self.frames.pop() {
                        let item = Item {
                            task: self.task.take(),
                            blocks,
                        };
                        match self.frames.last_mut() {
                            Some(Frame::List { items, .. }) => items.push(item),
                            _ => {
                                // Item outside a list (shouldn't happen); keep content.
                                let Item { blocks, .. } = &item;
                                let blocks = blocks.clone();
                                self.push_block(Block::List {
                                    start: None,
                                    items: vec![Item {
                                        task: item.task,
                                        blocks,
                                    }],
                                });
                            }
                        }
                    }
                }
            }
            TagEnd::CodeBlock => {
                if let Some((lang, text)) = self.code.take() {
                    self.push_block(Block::Code { lang, text });
                }
            }
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Strikethrough => self.style.strike = false,
            TagEnd::Link => self.style.link = None,
            TagEnd::Image => {
                if let Some((url, alt)) = self.image.take() {
                    let img = Inline::Image { alt, url };
                    if let Some(table) = &mut self.table {
                        if let Some(cell) = &mut table.cell {
                            cell.push(img);
                        } else {
                            self.inlines.push(img);
                        }
                    } else {
                        self.inlines.push(img);
                    }
                }
            }
            TagEnd::HtmlBlock => {
                if let Some(html) = self.html.take() {
                    self.push_block(Block::Html(html));
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    let TableBuild { head, rows, .. } = t;
                    self.push_block(Block::Table { head, rows });
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = &mut self.table {
                    t.head = std::mem::take(&mut t.row);
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = &mut self.table {
                    let row = std::mem::take(&mut t.row);
                    if !row.is_empty() {
                        t.rows.push(row);
                    }
                }
            }
            TagEnd::TableCell => {
                if let Some(t) = &mut self.table {
                    if let Some(cell) = t.cell.take() {
                        t.row.push(cell);
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush_inlines();
        while let Some(frame) = self.frames.pop() {
            match frame {
                Frame::Quote(blocks) => self.root.push(Block::Quote(blocks)),
                Frame::List { start, items } => self.root.push(Block::List { start, items }),
                Frame::Item(blocks) => self.root.push(Block::List {
                    start: None,
                    items: vec![Item { task: None, blocks }],
                }),
            }
        }
        self.root
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Parse a markdown document into a renderable block tree.
pub fn parse(source: &str) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let mut builder = Builder::default();
    for event in Parser::new_ext(source, options) {
        builder.event(event);
    }
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> Vec<Block> {
        parse(source)
    }

    #[test]
    fn heading_and_bold_italic() {
        let blocks = run("# Title\n\nHello **bold** and *it* world");
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }));
        match &blocks[1] {
            Block::Paragraph(inlines) => {
                // "Hello ", "bold", " and ", "it", " world"
                assert_eq!(inlines.len(), 5);
                let bold = &inlines[1];
                match bold {
                    Inline::Run { text, style } => {
                        assert_eq!(text, "bold");
                        assert!(style.bold && !style.italic);
                    }
                    other => panic!("expected run, got {other:?}"),
                }
                if let Inline::Run { style, .. } = &inlines[3] {
                    assert!(style.italic && !style.bold);
                } else {
                    panic!("expected run");
                }
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn code_block() {
        let blocks = run("```rust\nfn main() {}\n```\n");
        match &blocks[0] {
            Block::Code { lang, text } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(text, "fn main() {}\n");
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn nested_lists() {
        let blocks = run("- a\n  - b\n- c\n");
        match &blocks[0] {
            Block::List { start, items } => {
                assert_eq!(*start, None);
                assert_eq!(items.len(), 2);
                // first item contains a paragraph then a nested list
                match &items[0].blocks[1] {
                    Block::List { items: inner, .. } => assert_eq!(inner.len(), 1),
                    other => panic!("expected nested list, got {other:?}"),
                }
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn ordered_list_start() {
        let blocks = run("3. three\n4. four\n");
        match &blocks[0] {
            Block::List {
                start: Some(3),
                items,
            } => assert_eq!(items.len(), 2),
            other => panic!("expected ordered list, got {other:?}"),
        }
    }

    #[test]
    fn task_list() {
        let blocks = run("- [x] done\n- [ ] todo\n");
        match &blocks[0] {
            Block::List { items, .. } => {
                assert_eq!(items[0].task, Some(true));
                assert_eq!(items[1].task, Some(false));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn links_and_images() {
        let blocks = run("[site](https://example.com)\n\n![logo](img/logo.png)\n");
        match &blocks[0] {
            Block::Paragraph(inlines) => match &inlines[0] {
                Inline::Run { text, style } => {
                    assert_eq!(text, "site");
                    assert_eq!(style.link.as_deref(), Some("https://example.com"));
                }
                other => panic!("expected run, got {other:?}"),
            },
            other => panic!("expected paragraph, got {other:?}"),
        }
        match &blocks[1] {
            Block::Paragraph(inlines) => match &inlines[0] {
                Inline::Image { alt, url } => {
                    assert_eq!(alt, "logo");
                    assert_eq!(url, "img/logo.png");
                }
                other => panic!("expected image, got {other:?}"),
            },
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn table() {
        let blocks = run("| a | b |\n|---|---|\n| 1 | 2 |\n");
        match &blocks[0] {
            Block::Table { head, rows } => {
                assert_eq!(head.len(), 2);
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].len(), 2);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn blockquote() {
        let blocks = run("> quoted\n");
        match &blocks[0] {
            Block::Quote(inner) => assert_eq!(inner.len(), 1),
            other => panic!("expected quote, got {other:?}"),
        }
    }
}
