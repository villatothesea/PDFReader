//! A PDF's text layer → Markdown.
//!
//! Input is what the PDF reader serves for selection and search: per page,
//! the text in reading order with `\n` between lines and one box per char
//! (page points, top-left origin; a glyph's box spans its em, so its height
//! is the font size; inserted spaces and line breaks have empty boxes).
//!
//! From that geometry alone:
//! - **running headers and footers** (the same line in the top or bottom
//!   band of most pages, page numbers folded) are dropped;
//! - the **body size** is the most common font size (by characters);
//! - short lines set clearly larger are **headings**, one level per distinct
//!   size, largest first (`#` … `####`);
//! - lines opening with a bullet or a number are **list items**, and the
//!   indented lines under them continue the item;
//! - two or more consecutive lines split into the same number of columns by
//!   wide gaps are a **table** (the first row is the header);
//! - everything else is **paragraphs**: lines join while the gap to the next
//!   is a line gap rather than a paragraph gap, hyphenated breaks are
//!   rejoined, and a paragraph cut by a page break continues on the next
//!   page when it obviously does.
//!
//! What it can't see (bold, italics, images, reading order the PDF itself
//! gets wrong) it doesn't guess.
//!
//! MimiReader round 18: moved here from PDFInspector (`mireader.pdfinspector`,
//! retired) — "Convert to Markdown" is one of the PDF reader's own tools now,
//! and runs in the host's job process for the package, never the one serving
//! the open documents. It now says how far it has got and stops when asked.

use std::path::Path;

use mireader_reader_sdk::{Job, PageText, Reader as _, ReaderError, ToolOutput};

/// The action's id (`reader.toml`).
pub const ACTION: &str = "pdf-to-markdown";

/// Run the action: the Markdown of the job's PDF, a page at a time — its
/// progress said and a cancel heeded between pages.
pub fn run(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let path = job.path().to_path_buf();
    Ok(ToolOutput { mime: "text/markdown".into(), text: pdf_to_markdown_with(&path, job)?, ..ToolOutput::default() })
}

/// The Markdown of the PDF at `path`.
pub fn pdf_to_markdown(path: &Path) -> Result<String, ReaderError> {
    read_pages(path, &mut |_, _| Ok(()))
}

fn pdf_to_markdown_with(path: &Path, job: &mut Job<'_>) -> Result<String, ReaderError> {
    read_pages(path, &mut |done, total| {
        job.check_cancelled()?;
        job.progress(done, total, Some(format!("Page {} of {total}", done + 1)));
        Ok(())
    })
}

/// Read every page's text (calling `each(page, pages)` before each), then
/// lay it out. A PDF with no text layer at all is refused with the reason.
fn read_pages(path: &Path, each: &mut dyn FnMut(u64, u64) -> Result<(), ReaderError>) -> Result<String, ReaderError> {
    let mut doc = crate::PdfReader.open(path)?;
    let info = doc.info();
    let total = info.pages.len() as u64;
    let mut pages = Vec::with_capacity(info.pages.len());
    for (ix, size) in info.pages.iter().enumerate() {
        each(ix as u64, total)?;
        pages.push(Page { height: size.height, text: doc.page_text(ix as u32)? });
    }
    if pages.iter().all(|page| page.text.text.trim().is_empty()) {
        return Err(ReaderError::unsupported(
            "This PDF has no text layer (a scan or images only) — converting it needs OCR, which isn't supported yet.",
        ));
    }
    Ok(to_markdown(&pages))
}

/// One page of input.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Page height in points (for the header/footer bands).
    pub height: f32,
    pub text: PageText,
}

/// A gap this many ems wide between two glyphs splits a line into cells.
const CELL_GAP: f32 = 1.5;
/// Lines whose gap is under this many ems belong to one paragraph.
const LINE_GAP: f32 = 0.55;
/// A heading is set at least this much larger than the body.
const HEADING_RATIO: f32 = 1.15;
/// …and is at most this long.
const HEADING_MAX_CHARS: usize = 120;
/// Header / footer bands, as a share of the page height.
const BAND: f32 = 0.08;

#[derive(Debug, Clone)]
struct Cell {
    text: String,
}

#[derive(Debug, Clone)]
struct Line {
    page: usize,
    text: String,
    x0: f32,
    top: f32,
    bottom: f32,
    size: f32,
    cells: Vec<Cell>,
}

fn real(b: &[f32; 4]) -> bool {
    b[0] < b[2] && b[1] < b[3]
}

/// The text lines of one page, with their geometry. Lines without a single
/// drawn glyph are dropped.
fn lines_of(page_ix: usize, text: &PageText) -> Vec<Line> {
    let mut out = Vec::new();
    let mut chars: Vec<(char, [f32; 4])> = Vec::new();
    let boxes = text.boxes.iter().copied().chain(std::iter::repeat([0.0; 4]));
    for (ch, b) in text.text.chars().zip(boxes).chain(std::iter::once(('\n', [0.0; 4]))) {
        if ch != '\n' {
            chars.push((ch, b));
            continue;
        }
        if let Some(line) = finish_line(page_ix, &chars) {
            out.push(line);
        }
        chars.clear();
    }
    out
}

fn finish_line(page: usize, chars: &[(char, [f32; 4])]) -> Option<Line> {
    let drawn: Vec<[f32; 4]> = chars.iter().map(|(_, b)| *b).filter(real).collect();
    if drawn.is_empty() {
        return None;
    }
    let mut heights: Vec<f32> = drawn.iter().map(|b| b[3] - b[1]).collect();
    heights.sort_by(f32::total_cmp);
    let size = heights[heights.len() / 2];
    let mut cells: Vec<Cell> = Vec::new();
    let mut last_x1: Option<f32> = None;
    for (ch, b) in chars {
        if real(b) {
            if last_x1.is_none_or(|x1| b[0] - x1 > CELL_GAP * size) {
                cells.push(Cell { text: String::new() });
            }
            last_x1 = Some(b[2]);
        }
        if let Some(cell) = cells.last_mut() {
            cell.text.push(*ch);
        }
    }
    for cell in &mut cells {
        cell.text = cell.text.trim().to_string();
    }
    cells.retain(|cell| !cell.text.is_empty());
    let text: String = chars.iter().map(|(c, _)| *c).collect::<String>().trim().to_string();
    // Drawn but blank (a glyph that maps to whitespace).
    if text.is_empty() {
        return None;
    }
    Some(Line {
        page,
        text,
        x0: drawn.iter().map(|b| b[0]).fold(f32::MAX, f32::min),
        top: drawn.iter().map(|b| b[1]).fold(f32::MAX, f32::min),
        bottom: drawn.iter().map(|b| b[3]).fold(f32::MIN, f32::max),
        size,
        cells,
    })
}

/// A line with each run of digits folded to one `#`, for spotting the same
/// header on every page ("Manual - 4" ≈ "Manual - 12").
fn folded(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !out.ends_with('#') {
                out.push('#');
            }
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A table-of-contents entry: a title, a dot leader, a page number —
/// `(depth, title)`, the depth from a leading section number (`2.1.3` → 3).
fn toc_entry(text: &str) -> Option<(usize, String)> {
    let body = text.trim_end().trim_end_matches(|c: char| c.is_ascii_digit() || c.is_whitespace());
    if body.len() == text.trim_end().len() {
        return None; // no page number
    }
    let title = body.trim_end_matches(['.', '…', '·', ' ']);
    let leader = &body[title.len()..];
    if leader.chars().filter(|c| matches!(c, '.' | '…' | '·')).count() < 4 || title.trim().is_empty() {
        return None;
    }
    let first = title.split_whitespace().next().unwrap_or("");
    let numbered = first.split('.').filter(|s| !s.is_empty()).all(|s| s.chars().all(|c| c.is_ascii_digit()));
    let depth = if numbered { first.split('.').filter(|s| !s.is_empty()).count().max(1) } else { 1 };
    Some((depth.min(4), title.trim().to_string()))
}

/// "12", "Page 12", "12 / 40", "12 of 40", "- 12 -".
fn is_page_number(text: &str) -> bool {
    let lower = text.to_lowercase();
    let rest = lower.trim().trim_start_matches("page").trim_matches(|c: char| c == '-' || c == '–' || c.is_whitespace());
    !rest.is_empty()
        && rest.chars().any(|c| c.is_ascii_digit())
        && rest.split_whitespace().all(|w| w.chars().all(|c| c.is_ascii_digit() || c == '/') || w == "of")
}

/// Drop running headers / footers and bare page numbers. `body` is the
/// body size: a line in a band set smaller than it is a running header.
fn drop_furniture(lines: Vec<Line>, pages: &[Page], body: f32) -> Vec<Line> {
    let band = |line: &Line| -> Option<bool> {
        let height = pages.get(line.page).map_or(0.0, |p| p.height);
        if height <= 0.0 {
            None
        } else if line.bottom <= height * BAND {
            Some(true)
        } else if line.top >= height * (1.0 - BAND) {
            Some(false)
        } else {
            None
        }
    };
    let mut seen: std::collections::HashMap<(bool, String), std::collections::HashSet<usize>> = Default::default();
    for line in &lines {
        if let Some(top) = band(line) {
            seen.entry((top, folded(&line.text))).or_default().insert(line.page);
        }
    }
    // On two pages or more: a running header names the chapter it is in, so
    // it repeats across that chapter's pages, not the whole document's.
    let repeated = |line: &Line, top: bool| {
        pages.len() >= 3 && seen.get(&(top, folded(&line.text))).is_some_and(|on| on.len() >= 2)
    };
    lines
        .into_iter()
        .filter(|line| match band(line) {
            Some(top) => !(is_page_number(&line.text) || repeated(line, top) || line.size < body * 0.95),
            None => true,
        })
        .collect()
}

/// The most common font size, by characters (rounded to half points).
fn body_size(lines: &[Line]) -> f32 {
    let mut counts: std::collections::HashMap<i32, usize> = Default::default();
    for line in lines {
        *counts.entry((line.size * 2.0).round() as i32).or_default() += line.text.chars().count();
    }
    counts.into_iter().max_by_key(|(size, n)| (*n, -size)).map_or(12.0, |(size, _)| size as f32 / 2.0)
}

/// A list marker opening `text`: `(marker, rest)`, the marker already in
/// Markdown form (`- ` or `3. `).
fn list_marker(text: &str) -> Option<(String, String)> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if matches!(first, '•' | '●' | '▪' | '■' | '◦' | '‣' | '∙' | '·' | '–' | '—' | '-' | '*' | '○' | '►' | '▸') {
        let rest = chars.as_str();
        if rest.starts_with(char::is_whitespace) && !rest.trim().is_empty() {
            return Some(("- ".into(), rest.trim().to_string()));
        }
        return None;
    }
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    if (1..=3).contains(&digits.len()) {
        let after = &text[digits.len()..];
        if let Some(rest) = after.strip_prefix(['.', ')'])
            && rest.starts_with(char::is_whitespace)
            && !rest.trim().is_empty()
        {
            return Some((format!("{digits}. "), rest.trim().to_string()));
        }
    }
    None
}

/// Join `next` onto `text` as running prose: a word hyphenated across the
/// break is rejoined, otherwise a space.
fn join(text: &mut String, next: &str) {
    let hyphenated = text.ends_with('-')
        && text.chars().rev().nth(1).is_some_and(char::is_alphabetic)
        && next.chars().next().is_some_and(char::is_lowercase);
    if hyphenated {
        text.pop();
    } else if !text.is_empty() {
        text.push(' ');
    }
    text.push_str(next);
}

/// Escape what would turn a paragraph's start into other Markdown.
fn escape_start(text: &str) -> String {
    match text.chars().next() {
        Some('#' | '>' | '+' | '=' | '|') => format!("\\{text}"),
        _ => text.to_string(),
    }
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end().ends_with(['.', '!', '?', ':', ';', '"', '”', ')', '。', '！', '？'])
}

#[derive(Debug)]
enum Block {
    Heading(usize, String),
    Item { marker: String, text: String, x0: f32 },
    Paragraph(String),
    Table(Vec<Vec<String>>),
}

/// The Markdown for the pages' text.
pub fn to_markdown(pages: &[Page]) -> String {
    let lines: Vec<Line> = pages.iter().enumerate().flat_map(|(ix, page)| lines_of(ix, &page.text)).collect();
    if lines.is_empty() {
        return String::new();
    }
    let lines = drop_furniture(lines.clone(), pages, body_size(&lines));
    if lines.is_empty() {
        return String::new();
    }
    let body = body_size(&lines);
    let is_heading = |line: &Line| {
        line.size >= body * HEADING_RATIO && line.text.chars().count() <= HEADING_MAX_CHARS && list_marker(&line.text).is_none()
    };
    // Heading levels: one per distinct size, largest first.
    let mut sizes: Vec<i32> = lines.iter().filter(|l| is_heading(l)).map(|l| (l.size * 2.0).round() as i32).collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    sizes.dedup();
    let level = |line: &Line| {
        let size = (line.size * 2.0).round() as i32;
        sizes.iter().position(|s| *s == size).map_or(4, |ix| ix + 1).min(4)
    };

    let mut blocks: Vec<Block> = Vec::new();
    let mut ix = 0;
    while ix < lines.len() {
        let line = &lines[ix];
        let prev = ix.checked_sub(1).map(|p| &lines[p]);
        let gap = prev.filter(|p| p.page == line.page).map(|p| line.top - p.bottom);
        let close = |prev: &Line| gap.is_some_and(|g| g < LINE_GAP * prev.size.max(line.size)) && (prev.size - line.size).abs() < 0.15 * line.size;

        // Contents: a nested list of the titles (page numbers mean nothing
        // in the Markdown).
        if let Some((depth, title)) = toc_entry(&line.text) {
            blocks.push(Block::Item { marker: format!("{}- ", "  ".repeat(depth - 1)), text: title, x0: line.x0 });
            ix += 1;
            continue;
        }
        // A table: two or more consecutive lines with the same 2+ columns.
        if line.cells.len() >= 2 {
            let columns = line.cells.len();
            let run = lines[ix..]
                .iter()
                .take_while(|l| l.page == line.page && l.cells.len() == columns && !is_heading(l) && toc_entry(&l.text).is_none())
                .count();
            if run >= 2 {
                let rows = lines[ix..ix + run]
                    .iter()
                    .map(|l| l.cells.iter().map(|c| c.text.replace('|', "\\|")).collect())
                    .collect();
                blocks.push(Block::Table(rows));
                ix += run;
                continue;
            }
        }
        if is_heading(line) {
            let level = level(line);
            match blocks.last_mut() {
                // A heading set over two lines.
                Some(Block::Heading(l, text)) if *l == level && prev.is_some_and(|p| is_heading(p) && close(p)) => {
                    join(text, &line.text)
                }
                _ => blocks.push(Block::Heading(level, line.text.clone())),
            }
            ix += 1;
            continue;
        }
        if let Some((marker, text)) = list_marker(&line.text) {
            blocks.push(Block::Item { marker, text, x0: line.x0 });
            ix += 1;
            continue;
        }
        let joined = match (blocks.last_mut(), prev) {
            // An item's wrapped lines sit indented under its text.
            (Some(Block::Item { text, x0, .. }), Some(p))
                if line.x0 > *x0 + 0.3 * line.size
                    // …on this page, or the next one, mid-sentence.
                    && (close(p) || (p.page + 1 == line.page && !ends_sentence(text))) =>
            {
                join(text, &line.text);
                true
            }
            (Some(Block::Paragraph(text)), Some(p)) if !is_heading(p) && list_marker(&p.text).is_none() => {
                // A first-line indent after a full stop starts a new one.
                let indented = line.x0 > p.x0 + line.size && ends_sentence(&p.text);
                // Across a page break: an unfinished sentence going on in
                // lower case.
                let continues = p.page + 1 == line.page
                    && !ends_sentence(text)
                    && line.text.chars().next().is_some_and(char::is_lowercase);
                if (close(p) && !indented) || continues {
                    join(text, &line.text);
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if !joined {
            blocks.push(Block::Paragraph(line.text.clone()));
        }
        ix += 1;
    }

    let mut out = String::new();
    for (n, block) in blocks.iter().enumerate() {
        // Items of one list (bullets, or numbers) stay together; every other
        // block is apart.
        let tight = n > 0
            && matches!((&blocks[n - 1], block), (Block::Item { marker: a, .. }, Block::Item { marker: b, .. })
                if (a.trim_start() == "- ") == (b.trim_start() == "- "));
        if n > 0 {
            out.push_str(if tight { "\n" } else { "\n\n" });
        }
        match block {
            Block::Heading(level, text) => {
                out.push_str(&"#".repeat(*level));
                out.push(' ');
                out.push_str(text);
            }
            Block::Item { marker, text, .. } => {
                out.push_str(marker);
                out.push_str(text);
            }
            Block::Paragraph(text) => out.push_str(&escape_start(text)),
            Block::Table(rows) => {
                let row = |cells: &[String]| format!("| {} |", cells.join(" | "));
                out.push_str(&row(&rows[0]));
                out.push('\n');
                out.push_str(&row(&vec!["---".to_string(); rows[0].len()]));
                for cells in &rows[1..] {
                    out.push('\n');
                    out.push_str(&row(cells));
                }
            }
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One line: each char half an em wide from `x`, spaces drawn as
    /// nothing (an empty box, as the reader inserts them).
    fn line(text: &str, x: f32, top: f32, size: f32) -> (String, Vec<[f32; 4]>) {
        cells(&[(text, x)], top, size)
    }

    /// One line made of runs starting at the given x positions.
    fn cells(runs: &[(&str, f32)], top: f32, size: f32) -> (String, Vec<[f32; 4]>) {
        let mut text = String::new();
        let mut boxes = Vec::new();
        for (n, (run, x)) in runs.iter().enumerate() {
            if n > 0 {
                text.push(' ');
                boxes.push([0.0; 4]);
            }
            let mut at = *x;
            for ch in run.chars() {
                text.push(ch);
                if ch == ' ' {
                    boxes.push([at, top, at, top]);
                } else {
                    boxes.push([at, top, at + size * 0.5, top + size]);
                }
                at += size * 0.5;
            }
        }
        (text, boxes)
    }

    fn page(lines: &[(String, Vec<[f32; 4]>)]) -> Page {
        let mut text = PageText::default();
        for (n, (t, b)) in lines.iter().enumerate() {
            if n > 0 {
                text.text.push('\n');
                text.boxes.push([0.0; 4]);
            }
            text.text.push_str(t);
            text.boxes.extend(b);
        }
        Page { height: 800.0, text }
    }

    #[test]
    fn headings_paragraphs_and_lists() {
        let p = page(&[
            line("User Guide", 72.0, 60.0, 24.0),
            line("Getting started", 72.0, 100.0, 18.0),
            line("This guide shows how the kit is set", 72.0, 130.0, 12.0),
            line("up and played.", 72.0, 144.4, 12.0),
            line("A second paragraph follows a gap.", 72.0, 172.0, 12.0),
            line("• Snare drum", 72.0, 200.0, 12.0),
            line("• Kick drum with a long name that", 72.0, 214.4, 12.0),
            line("wraps under its text", 84.0, 228.8, 12.0),
            line("1. Tune it", 72.0, 256.0, 12.0),
            line("2) Play it", 72.0, 270.4, 12.0),
            line("A closing line, well below the list.", 72.0, 300.0, 12.0),
            line("And more body text to make 12 pt the body size of this page.", 72.0, 314.4, 12.0),
        ]);
        assert_eq!(
            to_markdown(&[p]),
            "# User Guide\n\n\
             ## Getting started\n\n\
             This guide shows how the kit is set up and played.\n\n\
             A second paragraph follows a gap.\n\n\
             - Snare drum\n\
             - Kick drum with a long name that wraps under its text\n\n\
             1. Tune it\n\
             2. Play it\n\n\
             A closing line, well below the list. And more body text to make 12 pt the body size of this page.\n"
        );
    }

    #[test]
    fn aligned_columns_make_a_table() {
        let p = page(&[
            line("The sounds included:", 72.0, 100.0, 12.0),
            cells(&[("Name", 72.0), ("Size", 200.0), ("Mics", 300.0)], 120.0, 12.0),
            cells(&[("Ludwig | 70s", 72.0), ("14\"", 200.0), ("6", 300.0)], 134.4, 12.0),
            cells(&[("Gretsch", 72.0), ("13\"", 200.0), ("4", 300.0)], 148.8, 12.0),
            line("After the table, a paragraph of ordinary running text.", 72.0, 180.0, 12.0),
        ]);
        assert_eq!(
            to_markdown(&[p]),
            "The sounds included:\n\n\
             | Name | Size | Mics |\n\
             | --- | --- | --- |\n\
             | Ludwig \\| 70s | 14\" | 6 |\n\
             | Gretsch | 13\" | 4 |\n\n\
             After the table, a paragraph of ordinary running text.\n"
        );
    }

    #[test]
    fn running_headers_footers_and_page_numbers_go() {
        let pages: Vec<Page> = (1..=4)
            .map(|n| {
                page(&[
                    line("ACME Drum Library — Manual", 72.0, 20.0, 9.0),
                    line(&format!("Body text of page {n} in the ordinary size."), 72.0, 300.0, 12.0),
                    line(&format!("{n}"), 300.0, 770.0, 9.0),
                ])
            })
            .collect();
        let md = to_markdown(&pages);
        assert!(!md.contains("ACME"), "{md}");
        assert!(!md.lines().any(|l| l.trim().parse::<u32>().is_ok()), "{md}");
        assert_eq!(md.matches("Body text of page").count(), 4);
    }

    /// A small running header seen once (a section's only page) still goes;
    /// a list item cut by a page break goes on at its hanging indent.
    #[test]
    fn small_band_headers_go_and_items_cross_pages() {
        let one = page(&[
            line("Basic Navigation", 480.0, 20.0, 9.0),
            line("Body text in the ordinary size on this page.", 72.0, 600.0, 12.0),
            line("• Kick Out: a long item that runs to the", 72.0, 700.0, 12.0),
            line("end of the page and then on", 84.0, 714.4, 12.0),
        ]);
        let two = page(&[line("NS10 with SSL9k Preamp.", 84.0, 72.0, 12.0)]);
        assert_eq!(
            to_markdown(&[one, two]),
            "Body text in the ordinary size on this page.\n\n\
             - Kick Out: a long item that runs to the end of the page and then on NS10 with SSL9k Preamp.\n"
        );
    }

    #[test]
    fn hyphenation_and_page_breaks_rejoin_prose() {
        let one = page(&[
            line("Paragraphs keep going when a word is hyph-", 72.0, 700.0, 12.0),
            line("enated and when the sentence runs over the", 72.0, 714.4, 12.0),
        ]);
        let two = page(&[
            line("page break, as this one does.", 72.0, 72.0, 12.0),
            line("Then Lower-case-proof: a new sentence after a stop.", 72.0, 120.0, 12.0),
        ]);
        assert_eq!(
            to_markdown(&[one, two]),
            "Paragraphs keep going when a word is hyphenated and when the sentence runs over the page break, as this one does.\n\n\
             Then Lower-case-proof: a new sentence after a stop.\n"
        );
    }

    /// A contents page: dot leaders and page numbers go, section numbers
    /// nest the list; "Manual - 4" and "Manual - 12" are the same footer.
    #[test]
    fn contents_pages_become_a_nested_list() {
        let pages: Vec<Page> = [4, 12, 13]
            .iter()
            .map(|n| {
                page(&[
                    line("1 Introduction ........................6", 62.0, 95.0, 11.0),
                    line("2.1 The Studios .................... 8", 62.0, 112.0, 11.0),
                    line("2.1.1 Studio Three ................ 8", 62.0, 129.0, 11.0),
                    line(&format!("Drummer MKII - Manual - {n}"), 300.0, 780.0, 10.0),
                ])
            })
            .collect();
        let md = to_markdown(&pages[..1].iter().cloned().chain(pages[1..].iter().cloned()).collect::<Vec<_>>());
        assert!(!md.contains("Manual -"), "{md}");
        assert!(md.starts_with("- 1 Introduction\n  - 2.1 The Studios\n    - 2.1.1 Studio Three\n"), "{md}");
        assert_eq!(toc_entry("Chapter 12"), None);
        assert_eq!(toc_entry("See page 12"), None);
    }

    #[test]
    fn a_page_number_is_recognised() {
        for text in ["12", "Page 12", "12 / 40", "12 of 40", "- 12 -", "page 3"] {
            assert!(is_page_number(text), "{text}");
        }
        for text in ["Chapter 12", "12 bars", "Page"] {
            assert!(!is_page_number(text), "{text}");
        }
    }

    #[test]
    fn paragraph_starts_are_escaped() {
        let p = page(&[line("# not a heading, just a hash sign", 72.0, 100.0, 12.0)]);
        assert_eq!(to_markdown(&[p]), "\\# not a heading, just a hash sign\n");
    }
}

