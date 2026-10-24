//! Page organising — extract, reorder, split and merge, each
//! a new PDF beside the original (the original is never changed). Built on
//! pdfcraft's organiser: pages are copied with their resources, form fields,
//! the document's information and, when merging, a bookmark per source.
//!
//! Page numbers come from the user as written (`1-3, 7, 10-`, see
//! [`parse_pages`]); file names say what is inside (`book (pages 1-3).pdf`).
//!
//! Round 17 (O6): moved here from PDFInspector (round 13, D3) — reading a
//! PDF and organising its pages are one package's work; the inspector keeps
//! conversion and inspection.
//!
//! Round 18 (#97): what points at pages follows them. pdfcraft's page copy
//! already maps links and GoTo actions to the copied pages (named
//! destinations become explicit; a link to a page left out is dropped).
//! What it didn't carry were the bookmarks: a reorder lost every one, and
//! so did an extract or a split. Now a reorder permutes the document's own
//! pages, so everything stays (bookmarks, the named-destination tree, page
//! labels, attachments). Extract and split carry the bookmarks that open a
//! page they keep, nested as they were under the nearest kept ancestor.

use std::{collections::HashMap, path::Path, sync::Arc};

use mireader_reader_sdk::{Job, ReaderError, ToolOutput, parse_pages};
use pdfcraft_cos::{Document, SaveOptions, write_full};
use pdfcraft_organize::{SplitBy, add_bookmark_at, bookmarks, combine, extract_pages, move_pages, page_count, split_ranges};

pub const EXTRACT: &str = "extract-pages";
pub const REORDER: &str = "reorder-pages";
pub const SPLIT_EVERY: &str = "split-every";
pub const SPLIT_AT: &str = "split-at";
pub const MERGE: &str = "merge";

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn stem_of(path: &Path) -> String {
    path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into())
}

fn open(path: &Path) -> Result<Document, ReaderError> {
    let bytes = std::fs::read(path)?;
    Document::open(Arc::new(bytes)).map_err(|error| ReaderError::invalid(format!("{} can't be read: {error}", name_of(path))))
}

fn pages_of(doc: &Document, path: &Path) -> Result<usize, ReaderError> {
    page_count(doc).map_err(|error| ReaderError::invalid(format!("{}: {error}", name_of(path))))
}

fn failed(error: impl std::fmt::Display) -> ReaderError {
    ReaderError::invalid(error.to_string())
}

/// Write `doc` into the job's folder as `name`; its path for the output.
fn write(job: &Job<'_>, doc: &Document, name: &str) -> Result<String, ReaderError> {
    let bytes = write_full(doc, &SaveOptions::default()).map_err(failed)?;
    let path = job.out_file(name)?;
    std::fs::write(&path, bytes)?;
    Ok(path.to_string_lossy().into_owned())
}

/// Round 18 (#97): `pages` of `src` as a new document, with the bookmarks
/// that open one of them.
fn extract(src: &Document, pages: &[usize]) -> Result<Document, ReaderError> {
    let mut part = extract_pages(src, pages).map_err(failed)?;
    carry_bookmarks(src, &mut part, pages)?;
    Ok(part)
}

/// Copy into `part` the bookmarks of `src` that open one of `pages` (its
/// pages, in `part`'s order), keeping their nesting under the nearest
/// bookmark that came along and their place on the page (`/XYZ`).
fn carry_bookmarks(src: &Document, part: &mut Document, pages: &[usize]) -> Result<(), ReaderError> {
    let mut new_index: HashMap<usize, usize> = HashMap::new();
    for (at, page) in pages.iter().enumerate() {
        new_index.entry(*page).or_insert(at);
    }
    // Source path → path in `part`.
    let mut placed: HashMap<Vec<usize>, Vec<usize>> = HashMap::new();
    for mark in bookmarks(src) {
        let Some(page) = mark.page.and_then(|page| new_index.get(&page).copied()) else { continue };
        let parent = (0..mark.path.len()).rev().find_map(|len| placed.get(&mark.path[..len])).cloned().unwrap_or_default();
        let (left, top) = match &mark.view {
            Some((kind, numbers)) if kind.as_slice() == b"XYZ" => {
                (numbers.first().copied().flatten(), numbers.get(1).copied().flatten())
            }
            _ => (None, None),
        };
        let path = add_bookmark_at(part, &parent, usize::MAX, &mark.title, page, left, top).map_err(failed)?;
        placed.insert(mark.path, path);
    }
    Ok(())
}

/// Round 18 (#97): put `doc`'s own pages in `order` (a permutation of all
/// of them) — the pages stay the same objects, so everything that points
/// at them keeps pointing at them.
fn permute(doc: &mut Document, order: &[usize]) -> Result<(), ReaderError> {
    let mut now: Vec<usize> = (0..order.len()).collect();
    for (at, page) in order.iter().enumerate() {
        let from = now.iter().position(|p| p == page).ok_or_else(|| failed(format!("no page {}", page + 1)))?;
        if from != at {
            move_pages(doc, &[from], at).map_err(failed)?;
            let moved = now.remove(from);
            now.insert(at, moved);
        }
    }
    Ok(())
}

/// `1-3` for a run of pages (0-based in, 1-based out).
fn span(first: usize, last: usize) -> String {
    if first == last { format!("{}", first + 1) } else { format!("{}-{}", first + 1, last + 1) }
}

pub fn run(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let source = job.path().to_path_buf();
    let stem = stem_of(&source);
    let files = match job.action {
        EXTRACT => {
            let doc = open(&source)?;
            let pages = parse_pages(job.text("pages")?, pages_of(&doc, &source)?)?;
            job.check_cancelled()?;
            let label: String = job.text("pages")?.split_whitespace().collect::<Vec<_>>().join(" ");
            let part = extract(&doc, &pages)?;
            vec![write(job, &part, &format!("{stem} (pages {label}).pdf"))?]
        }
        REORDER => {
            let mut doc = open(&source)?;
            let count = pages_of(&doc, &source)?;
            let mut order = parse_pages(job.text("order")?, count)?;
            let mut seen = vec![false; count];
            for &page in &order {
                if std::mem::replace(&mut seen[page], true) {
                    return Err(ReaderError::bad_request(format!("page {} is listed twice", page + 1)));
                }
            }
            // Pages not listed follow, in their own order: nothing is lost.
            order.extend((0..count).filter(|page| !seen[*page]));
            job.check_cancelled()?;
            permute(&mut doc, &order)?;
            vec![write(job, &doc, &format!("{stem} (reordered).pdf"))?]
        }
        SPLIT_EVERY | SPLIT_AT => {
            let doc = open(&source)?;
            let count = pages_of(&doc, &source)?;
            let by = if job.action == SPLIT_EVERY {
                SplitBy::PageCount(job.integer("every")?.max(1) as usize)
            } else {
                SplitBy::Before(parse_pages(job.text("at")?, count)?)
            };
            let ranges = split_ranges(count, &by);
            let mut files = Vec::new();
            for (n, range) in ranges.iter().enumerate() {
                job.check_cancelled()?;
                job.progress(n as u64, ranges.len() as u64, Some(format!("Part {} of {}", n + 1, ranges.len())));
                let part = extract(&doc, &range.clone().collect::<Vec<_>>())?;
                files.push(write(job, &part, &format!("{stem} (pages {}).pdf", span(range.start, range.end - 1)))?);
            }
            files
        }
        MERGE => {
            let total = job.inputs.len() as u64;
            let mut docs = Vec::new();
            for (n, path) in job.inputs.clone().iter().enumerate() {
                job.check_cancelled()?;
                job.progress(n as u64, total + 1, Some(format!("Reading {}", name_of(path))));
                docs.push((stem_of(path), open(path)?));
            }
            job.check_cancelled()?;
            job.progress(total, total + 1, Some("Merging".into()));
            let sources: Vec<(&str, &Document)> = docs.iter().map(|(title, doc)| (title.as_str(), doc)).collect();
            let merged = combine(&sources).map_err(failed)?;
            vec![write(job, &merged, &format!("{stem} (merged).pdf"))?]
        }
        other => return Err(ReaderError::bad_request(format!("no action {other:?}"))),
    };
    Ok(ToolOutput { files, ..ToolOutput::default() })
}
