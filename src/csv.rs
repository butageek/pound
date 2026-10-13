//! CSV rendering input: delimited text -> a sanitized, sortable table.
//!
//! Same contract as [`crate::markdown`]: a pure function from the file's
//! bytes to sanitized HTML — no UI types here, so the whole pipeline
//! (delimiter sniffing, RFC-4180 quoting, sorting, escaping) is
//! unit-tested headlessly.
//!
//! - The delimiter is sniffed per file: the most common of `,` `;` tab
//!   `|` on the first line, outside quoted fields (Excel in European
//!   locales exports semicolon-separated files; `,` wins ties).
//! - The first row is the header (sticky, clickable to sort).
//! - Sorting stays in the unidirectional flow: a header click posts
//!   `csv-sort\n<column>` over ipc, the model re-renders the stored grid
//!   under the new sort, the view pushes the new table. Columns where
//!   every non-empty value parses as a number sort numerically;
//!   everything else sorts as text. Empty cells always sink, whichever
//!   way the sort points.
//! - Cells are HTML-escaped before the table is built and the whole
//!   result is sanitized with ammonia — a CSV can carry arbitrary text,
//!   and file content must never execute (same rule as raw HTML in
//!   markdown).
//! - Wide files render the first [`MAX_ROWS`] data rows plus a visible
//!   note; rendering a million rows would hang the WebView instead of
//!   reading. The status bar keeps the true row/column counts.

use std::cmp::Reverse;
use std::fmt::Write as _;

/// How many data rows render at most (the note states the true count).
pub const MAX_ROWS: usize = 10_000;

/// The sort state of a CSV document (render state: the table is
/// generated under it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: usize,
    pub ascending: bool,
}

/// Render delimited text as a sanitized HTML table, plus the file's
/// shape — `(data rows, columns)` — or `None` when there is not even a
/// header row. The count is always the whole file's, never the rendered
/// cap.
pub fn to_html(source: &str, sort: Option<Sort>) -> (String, Option<(usize, usize)>) {
    grid_to_html(&parse_grid(source), sort)
}

/// Parse delimited text into a grid of string cells: BOM stripped,
/// delimiter sniffed, ragged rows kept.
pub fn parse_grid(source: &str) -> Vec<Vec<String>> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(sniff_delimiter(source))
        .has_headers(false)
        .flexible(true)
        .from_reader(source.as_bytes());
    let mut rows = Vec::new();
    for record in reader.records() {
        match record {
            Ok(record) => rows.push(record.iter().map(|f| f.to_owned()).collect()),
            Err(_) => break, // in-memory input + flexible: practically unreachable
        }
    }
    rows
}

/// Render a parsed grid as a sanitized, sorted HTML table plus its
/// shape — `(data rows, columns)`, the whole grid's, never the rendered
/// cap. Sorting works on a copy: the stored grid is shared state.
pub fn grid_to_html(rows: &[Vec<String>], sort: Option<Sort>) -> (String, Option<(usize, usize)>) {
    let mut rows = rows.to_vec();
    // The shape exists once there is at least a header row.
    let shape = (!rows.is_empty()).then(|| {
        (
            rows.len().saturating_sub(1),
            rows.iter().map(|r| r.len()).max().unwrap_or(0),
        )
    });
    if let Some(sort) = sort {
        if let Some((_header, data)) = rows.split_first_mut() {
            sort_rows(data, sort);
        }
    }
    let html = sanitize(&render(&rows, sort));
    (html, shape)
}

/// Pick the delimiter: count the candidates on the first line, outside
/// quoted fields; the earliest (`,`) wins ties and is the fallback when
/// none appear.
fn sniff_delimiter(source: &str) -> u8 {
    const CANDIDATES: [u8; 4] = *b",;\t|";
    let mut counts = [0usize; 4];
    let mut quoted = false;
    for &b in source.as_bytes() {
        match b {
            b'\n' => break,
            b'"' => quoted = !quoted,
            _ if !quoted => {
                if let Some(i) = CANDIDATES.iter().position(|&c| c == b) {
                    counts[i] += 1;
                }
            }
            _ => {}
        }
    }
    counts
        .iter()
        .enumerate()
        .max_by_key(|&(i, &n)| (n, Reverse(i)))
        .filter(|&(_, &n)| n > 0)
        .map_or(b',', |(i, _)| CANDIDATES[i])
}

/// Sort data rows by `column`: numerically when every non-empty value in
/// the column parses as a number, else as text. Stable, so ties keep
/// file order; empty cells sink regardless of direction.
fn sort_rows(rows: &mut [Vec<String>], sort: Sort) {
    let numeric = rows.iter().all(|row| {
        let value = field(row, sort.column).trim();
        value.is_empty() || value.parse::<f64>().is_ok()
    });
    rows.sort_by(|a, b| {
        let (av, bv) = (field(a, sort.column), field(b, sort.column));
        match (av.is_empty(), bv.is_empty()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => {
                let ord = if numeric {
                    av.trim()
                        .parse::<f64>()
                        .unwrap_or(f64::NAN)
                        .partial_cmp(&bv.trim().parse::<f64>().unwrap_or(f64::NAN))
                        .unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    av.cmp(bv)
                };
                if sort.ascending {
                    ord
                } else {
                    ord.reverse()
                }
            }
        }
    });
}

/// Build the (pre-sanitization) table HTML; the shell adds interaction
/// (sort clicks, column resizing) to the live DOM, so this stays plain.
fn render(rows: &[Vec<String>], sort: Option<Sort>) -> String {
    let Some((header, data)) = rows.split_first() else {
        return "<div class=\"csv-empty\">This file has no rows.</div>".to_owned();
    };
    let shown = &data[..data.len().min(MAX_ROWS)];
    let columns = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let numeric: Vec<bool> = (0..columns).map(|c| column_is_numeric(data, c)).collect();

    let mut html = String::new();
    html.push_str("<table class=\"csv\"><colgroup>");
    for c in 0..columns {
        let _ = write!(
            html,
            "<col{}>",
            class_if("num", numeric.get(c).copied().unwrap_or(false))
        );
    }
    html.push_str("</colgroup><thead><tr>");
    for c in 0..columns {
        let mut classes: Vec<&str> = Vec::new();
        if numeric.get(c).copied().unwrap_or(false) {
            classes.push("num");
        }
        if let Some(sort) = sort.filter(|s| s.column == c) {
            classes.push(if sort.ascending {
                "sort-asc"
            } else {
                "sort-desc"
            });
        }
        let _ = write!(
            html,
            "<th{}>{}</th>",
            class_attr(&classes),
            escape(field(header, c))
        );
    }
    html.push_str("</tr></thead><tbody>");
    for row in shown {
        html.push_str("<tr>");
        for c in 0..columns {
            let _ = write!(
                html,
                "<td{}>{}</td>",
                class_if("num", numeric.get(c).copied().unwrap_or(false)),
                escape(field(row, c))
            );
        }
        html.push_str("</tr>");
    }
    html.push_str("</tbody></table>");
    if data.len() > shown.len() {
        let _ = write!(
            html,
            "<div class=\"csv-note\">Showing the first {} of {} rows.</div>",
            shown.len(),
            data.len()
        );
    }
    html
}

/// Whether column `c` sorts numerically: at least one non-empty value,
/// and every non-empty value parses as a number (missing cells in ragged
/// rows count as empty — an all-empty column makes no claim).
fn column_is_numeric(data: &[Vec<String>], column: usize) -> bool {
    let mut any_value = false;
    let all = data.iter().all(|row| {
        let value = field(row, column).trim();
        if value.is_empty() {
            true
        } else {
            any_value = true;
            value.parse::<f64>().is_ok()
        }
    });
    all && any_value
}

/// The value of `column` in `row` ("" for ragged short rows).
fn field(row: &[String], column: usize) -> &str {
    row.get(column).map_or("", |v| v.as_str())
}

/// `class="…"` for a non-empty list, else empty.
fn class_attr(classes: &[&str]) -> String {
    if classes.is_empty() {
        String::new()
    } else {
        format!(" class=\"{}\"", classes.join(" "))
    }
}

/// `class="…"` when `on`, else empty — the td/col class hook.
fn class_if(name: &str, on: bool) -> String {
    if on {
        format!(" class=\"{name}\"")
    } else {
        String::new()
    }
}

/// Escape text for HTML content. Ammonia remains the second line of
/// defense; this keeps a cell's text from breaking out of its element
/// while the table is being assembled.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Sanitize the generated table. Ammonia's default allowlist already
/// covers the table set (`table`/`thead`/`tbody`/`tr`/`th`/`td`/
/// `colgroup`/`col`/`div`); the classes are ours — numeric alignment and
/// the sort-state arrow — and inert (colors come from the shell CSS).
fn sanitize(raw: &str) -> String {
    ammonia::Builder::new()
        .add_tag_attributes("table", ["class"])
        .add_tag_attributes("th", ["class"])
        .add_tag_attributes("td", ["class"])
        .add_tag_attributes("col", ["class"])
        .add_tag_attributes("div", ["class"])
        .clean(raw)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_table_has_header_and_shape() {
        let (html, shape) = to_html("name,age\nada,36\nbob,25\n", None);
        assert_eq!(shape, Some((2, 2)));
        assert!(html.contains("<table class=\"csv\">"));
        assert!(html.contains("<th>name</th><th class=\"num\">age</th>"));
        assert!(html.contains("<td>ada</td><td class=\"num\">36</td>"));
    }

    #[test]
    fn empty_and_header_only_files() {
        let (html, shape) = to_html("", None);
        assert_eq!(shape, None);
        assert!(html.contains("csv-empty"));
        let (html, shape) = to_html("a,b\n", None);
        assert_eq!(shape, Some((0, 2)));
        assert!(html.contains("<th>a</th>"));
        assert!(!html.contains("<td"));
    }

    #[test]
    fn quoted_fields_and_special_characters_survive() {
        let (html, _) = to_html(
            "h1,h2,h3\n\"a, b\",\"line\nbreak\",\"say \"\"hi\"\"\"\nx,\"<script>alert(1)</script>\",'plain'\n",
            None,
        );
        // Quoted commas, newlines and doubled quotes stay in one cell.
        assert!(html.contains("<td>a, b</td>"));
        assert!(html.contains("line\nbreak"));
        assert!(html.contains("say \"hi\""));
        // File content never executes: it arrives escaped, and ammonia
        // keeps stripping structure it does not recognize.
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn delimiter_is_sniffed() {
        let (html, _) = to_html("a;b;c\nx;y;z\n", None);
        assert!(html.contains("<th>a</th><th>b</th><th>c</th>"));
        let (html, _) = to_html("a\tb\tc\nx\ty\tz\n", None);
        assert!(html.contains("<th>a</th><th>b</th><th>c</th>"));
        // No candidates at all: comma, and the line stays one column.
        let (html, shape) = to_html("just one column\nstill one\n", None);
        assert_eq!(shape, Some((1, 1)));
        assert!(html.contains("<th>just one column</th>"));
    }

    #[test]
    fn bom_is_stripped_from_the_first_header_cell() {
        let (html, _) = to_html("\u{feff}name,value\nada,1\n", None);
        assert!(html.contains("<th>name</th>"));
    }

    #[test]
    fn sorting_is_numeric_for_numeric_columns() {
        let source = "n,name\n10,ten\n2,two\n33,thirtythree\n";
        let (asc, _) = to_html(
            source,
            Some(Sort {
                column: 0,
                ascending: true,
            }),
        );
        let positions = ["2", "10", "33"]
            .iter()
            .map(|n| asc.find(&format!("<td class=\"num\">{n}</td>")))
            .collect::<Option<Vec<_>>>()
            .expect("all cells present");
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{asc}");
        let (desc, _) = to_html(
            source,
            Some(Sort {
                column: 0,
                ascending: false,
            }),
        );
        let positions = ["33", "10", "2"]
            .iter()
            .map(|n| desc.find(&format!("<td class=\"num\">{n}</td>")))
            .collect::<Option<Vec<_>>>()
            .expect("all cells present");
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{desc}");
        // The arrow rides on the sorted header.
        assert!(asc.contains("sort-asc"));
        assert!(desc.contains("sort-desc"));
    }

    #[test]
    fn sorting_is_textual_for_mixed_columns_and_empties_sink() {
        let source = "name,note\nb,\na,kept\n,\n";
        // `note` mixes "" and text: textual sort, empties last either way.
        let (html, _) = to_html(
            source,
            Some(Sort {
                column: 1,
                ascending: true,
            }),
        );
        assert!(html.contains("sort-asc"));
        let kept = html.find("kept").unwrap();
        let last_empty = html.rfind("<td></td>").unwrap();
        assert!(kept < last_empty, "{html}");
        let (html, _) = to_html(
            source,
            Some(Sort {
                column: 1,
                ascending: false,
            }),
        );
        let kept = html.find("kept").unwrap();
        let last_empty = html.rfind("<td></td>").unwrap();
        assert!(kept < last_empty, "{html}");
    }

    #[test]
    fn ragged_rows_do_not_panic() {
        let (html, shape) = to_html("a,b,c\n1\n2,3\n", None);
        assert_eq!(shape, Some((2, 3)));
        // Columns 0-1 are numeric ("1"/"2" and ""/"3"), the all-empty
        // column 2 makes no claim.
        assert!(html.contains("<td class=\"num\">1</td><td class=\"num\"></td><td></td>"));
    }

    #[test]
    fn wide_files_are_capped_with_a_note() {
        let mut source = String::from("n\n");
        for i in 0..=MAX_ROWS {
            let _ = writeln!(source, "{i}");
        }
        let (html, shape) = to_html(&source, None);
        // The shape counts everything; the table renders the cap.
        assert_eq!(shape, Some((MAX_ROWS + 1, 1)));
        assert!(html.contains("csv-note"));
        assert!(html.contains(&format!("of {} rows", MAX_ROWS + 1)));
        let rendered = html.matches("<tr><td").count();
        assert_eq!(rendered, MAX_ROWS);
    }

    #[test]
    fn crlf_files_parse_cleanly() {
        let (html, shape) = to_html("a,b\r\n1,2\r\n", None);
        assert_eq!(shape, Some((1, 2)));
        assert!(html.contains("<td class=\"num\">1</td><td class=\"num\">2</td>"));
    }
}
