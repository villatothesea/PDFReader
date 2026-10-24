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

use std::{path::Path, sync::Arc};

use mireader_reader_sdk::{Job, ReaderError, ToolOutput, parse_pages};
use pdfcraft_cos::{Document, SaveOptions, write_full};
use pdfcraft_organize::{SplitBy, combine, extract_pages, page_count, split_ranges};

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
            let part = extract_pages(&doc, &pages).map_err(failed)?;
            vec![write(job, &part, &format!("{stem} (pages {label}).pdf"))?]
        }
        REORDER => {
            let doc = open(&source)?;
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
            let reordered = extract_pages(&doc, &order).map_err(failed)?;
            vec![write(job, &reordered, &format!("{stem} (reordered).pdf"))?]
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
                let part = extract_pages(&doc, &range.clone().collect::<Vec<_>>()).map_err(failed)?;
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
