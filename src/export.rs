//! MimiReader round 19 (O12, T1 item 2): a PDF's pages as images, and its
//! text layer as plain text — two of the reader's own tools, run in the
//! host's job process for the package like the others.
//!
//! Both take the pages people write (`1-3, 7, 10-`, [`parse_pages`]); left
//! empty, every page.
//!
//! **Plain text** is the text layer, page by page in the order asked, a
//! form feed (`\f`, U+000C) between pages — pdftotext's convention: what
//! reads the file sees nothing, tools that page (`less`, `pr`, editors with
//! page breaks) see the page breaks, and page *n* is always after *n − 1*
//! form feeds, even when a page between has no text. A marker line ("—
//! Page 3 —") would read better in Notepad but would be text of ours inside
//! the user's, in one language. A PDF whose pages have no text at all
//! (a scan) is refused with the reason, never written as an empty file;
//! pages without text among ones with it are named in a note.
//!
//! **Images** are PNGs, one per page, `{stem}-{page}.png` with the page
//! number zero-padded to at least three digits (the document's last page
//! sets the width), so they sort in page order everywhere. 150 DPI unless
//! asked (a 40-page paper is a few MB), at most 300: at 300 an A4 or Letter
//! page is ~8.7 megapixels, under the renderer's 12-megapixel ceiling. A
//! larger page is rendered at the most the ceiling allows, and a note says
//! at what — it is never quietly smaller.

use std::path::Path;

use mireader_reader_sdk::{Job, Reader as _, ReaderError, ToolOutput, parse_pages};

pub const TO_TEXT: &str = "pdf-to-text";
pub const TO_IMAGES: &str = "pdf-to-images";

/// Images: the resolution unless asked, and the range allowed.
pub const DPI_DEFAULT: i64 = 150;
pub const DPI_MIN: i64 = 72;
pub const DPI_MAX: i64 = 300;

/// The page break between pages of the plain text.
pub const PAGE_BREAK: char = '\u{c}';

pub fn run(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    match job.action {
        TO_TEXT => to_text(job),
        TO_IMAGES => to_images(job),
        other => Err(ReaderError::bad_request(format!("no action {other:?}"))),
    }
}

fn stem_of(path: &Path) -> String {
    path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into())
}

/// The pages asked for (0-based, in the order written), or every page.
fn pages_wanted(job: &Job<'_>, count: usize) -> Result<Vec<usize>, ReaderError> {
    match job.text("pages").ok().map(str::trim).filter(|spec| !spec.is_empty()) {
        Some(spec) => parse_pages(spec, count),
        None if count == 0 => Err(ReaderError::invalid("This PDF has no pages.")),
        None => Ok((0..count).collect()),
    }
}

/// `3, 7-9` for 0-based pages, runs folded.
fn page_list(pages: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut ix = 0;
    while ix < pages.len() {
        let start = pages[ix];
        let mut end = start;
        while ix + 1 < pages.len() && pages[ix + 1] == end + 1 {
            ix += 1;
            end = pages[ix];
        }
        out.push(if start == end { format!("{}", start + 1) } else { format!("{}-{}", start + 1, end + 1) });
        ix += 1;
    }
    out.join(", ")
}

fn to_text(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let mut doc = crate::PdfReader.open(job.path())?;
    let pages = pages_wanted(job, doc.info().pages.len())?;
    let total = pages.len() as u64;
    let mut text = String::new();
    let mut empty = Vec::new();
    for (n, &page) in pages.iter().enumerate() {
        job.check_cancelled()?;
        job.progress(n as u64, total, Some(format!("Page {} of {total}", n + 1)));
        if n > 0 {
            text.push(PAGE_BREAK);
        }
        let page_text = doc.text(page as u32)?;
        let page_text = page_text.trim_end();
        if page_text.trim().is_empty() {
            empty.push(page);
        } else {
            text.push_str(page_text);
            text.push('\n');
        }
    }
    if empty.len() == pages.len() {
        return Err(ReaderError::unsupported(if pages.len() == 1 {
            "No text found: this page has no text layer (a scan or an image) — reading it needs OCR, which isn't supported yet."
        } else {
            "No text found: these pages have no text layer (a scan or images only) — reading them needs OCR, which isn't supported yet."
        }));
    }
    let mut notes = Vec::new();
    if !empty.is_empty() {
        let (word, list) = if empty.len() == 1 { ("page", page_list(&empty)) } else { ("pages", page_list(&empty)) };
        notes.push(format!("no text on {word} {list} (scans or images)"));
    }
    Ok(ToolOutput { mime: "text/plain".into(), text, notes, ..ToolOutput::default() })
}

fn to_images(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let dpi = job.integer("dpi").unwrap_or(DPI_DEFAULT).clamp(DPI_MIN, DPI_MAX);
    let stem = stem_of(job.path());
    let mut doc = crate::PdfReader.open(job.path())?;
    let sizes = doc.info().pages;
    let mut pages = pages_wanted(job, sizes.len())?;
    // One file per page: a page asked for twice is written once.
    let mut seen = vec![false; sizes.len()];
    pages.retain(|&page| !std::mem::replace(&mut seen[page], true));
    let width = sizes.len().to_string().len().max(3);
    let scale = dpi as f32 / 72.0;
    let total = pages.len() as u64;
    let mut files = Vec::with_capacity(pages.len());
    let mut lowered: Vec<(usize, i64)> = Vec::new();
    for (n, &page) in pages.iter().enumerate() {
        job.check_cancelled()?;
        job.progress(n as u64, total, Some(format!("Page {} of {total}", n + 1)));
        let rendered = doc.render_page(page as u32, scale)?;
        // The renderer's ceiling lowers the scale of a very large page.
        let size = sizes[page];
        let long_side = size.width.max(size.height);
        let got = rendered.width.max(rendered.height) as f32;
        if got + 1.5 < long_side * scale {
            lowered.push((page, (got / long_side * 72.0).floor() as i64));
        }
        let path = job.out_file(&format!("{stem}-{:0width$}.png", page + 1))?;
        std::fs::write(&path, &rendered.png)?;
        files.push(path.to_string_lossy().into_owned());
    }
    let notes = lowered
        .iter()
        .map(|(page, at)| format!("page {} is too large for {dpi} DPI: saved at {at} DPI", page + 1))
        .collect();
    Ok(ToolOutput { files, notes, ..ToolOutput::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_lists_fold_runs() {
        assert_eq!(page_list(&[2]), "3");
        assert_eq!(page_list(&[2, 6, 7, 8, 11]), "3, 7-9, 12");
    }
}
