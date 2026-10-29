//! Round 20 (#197): a clean copy — the whole file written anew.
//!
//! Incremental saves pile revisions on the original; this writes only the
//! objects the document still reaches, packed into object streams with a
//! cross-reference stream, as a new file beside it. Before anything is
//! written the copy is checked by an implementation other than the one that
//! wrote it: hayro (MimiReader's renderer, its own parser) reads both files
//! and every page's text must match, and pdfcraft reads the copy back. When
//! `qpdf` is installed it checks the written file as well. A copy that fails
//! a check is not kept.

use std::path::Path;
use std::sync::Arc;

use mireader_reader_sdk::{Document as _, ErrorCode, Job, ReaderError, ToolOutput};
use pdfcraft_cos::{Document as Cos, SaveOptions, write_full};

use crate::PdfDocument;

pub const ACTION: &str = "rewrite-clean";

fn failed(why: impl std::fmt::Display) -> ReaderError {
    ReaderError::invalid(format!("The clean copy didn't pass its check, so it wasn't kept: {why}"))
}

/// Every page's text as hayro reads it.
fn texts(doc: &mut PdfDocument, job: &mut Job<'_>, done: u64, total: u64) -> Result<Vec<String>, ReaderError> {
    let pages = doc.info().pages.len();
    let mut out = Vec::with_capacity(pages);
    for page in 0..pages {
        job.check_cancelled()?;
        job.progress(done + page as u64, total, Some("Checking the copy".into()));
        out.push(doc.page_text(page as u32)?.text);
    }
    Ok(out)
}

/// `qpdf --check` on `path`: `None` when qpdf isn't installed.
fn qpdf_check(path: &Path) -> Option<Result<(), String>> {
    let mut command = std::process::Command::new("qpdf");
    command.arg("--check").arg(path).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    Some(if output.status.success() {
        Ok(())
    } else {
        let said = String::from_utf8_lossy(&output.stdout).lines().chain(String::from_utf8_lossy(&output.stderr).lines()).last().unwrap_or("").to_string();
        Err(said.chars().take(200).collect())
    })
}

fn size(bytes: usize) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 { format!("{kb:.0} KB") } else { format!("{:.1} MB", kb / 1024.0) }
}

pub fn run(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let source = job.path().to_path_buf();
    let original = std::fs::read(&source)?;
    let doc = Cos::open(Arc::new(original.clone())).map_err(|error| ReaderError::invalid(format!("It can't be read: {error}")))?;
    if doc.security().is_some() && doc.permissions().is_none() {
        return Err(ReaderError::new(ErrorCode::Encrypted, "this PDF is password-protected"));
    }
    if doc.permissions().is_some_and(|p| !p.modify()) {
        return Err(ReaderError::new(ErrorCode::Unsupported, "This PDF's security settings don't allow it to be rewritten."));
    }
    job.progress(0, 3, Some("Writing the copy".into()));
    let clean = write_full(&doc, &SaveOptions::default()).map_err(|error| ReaderError::invalid(error.to_string()))?;
    job.check_cancelled()?;

    // pdfcraft reads its own copy back.
    let reread = Cos::open(Arc::new(clean.clone())).map_err(failed)?;
    if reread.reconstructed() {
        return Err(failed("its structure had to be rebuilt to read it"));
    }
    // hayro, the other implementation, reads both: the same pages, the same
    // text on each.
    let mut before = PdfDocument::new(original).map_err(|error| failed(error.message))?;
    let mut after = PdfDocument::new(clean.clone()).map_err(|error| failed(error.message))?;
    let pages = before.info().pages.len();
    if after.info().pages.len() != pages {
        return Err(failed(format!("it has {} pages, the original {pages}", after.info().pages.len())));
    }
    let total = 2 * pages as u64 + 1;
    let was = texts(&mut before, job, 0, total)?;
    let now = texts(&mut after, job, pages as u64, total)?;
    if let Some(page) = was.iter().zip(&now).position(|(a, b)| a != b) {
        return Err(failed(format!("the text of page {} differs", page + 1)));
    }

    let stem = source.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
    let out = job.out_file(&format!("{stem} (clean).pdf"))?;
    std::fs::write(&out, &clean)?;
    let mut notes = vec![format!("{} → {}", size(std::fs::metadata(&source).map(|m| m.len() as usize).unwrap_or(0)), size(clean.len()))];
    match qpdf_check(&out) {
        Some(Ok(())) => notes.push("checked by two PDF readers and by qpdf".into()),
        Some(Err(said)) => {
            let _ = std::fs::remove_file(&out);
            return Err(failed(format!("qpdf: {said}")));
        }
        None => notes.push(format!("checked by two PDF readers, {pages} pages and their text unchanged")),
    }
    job.progress(total, total, None);
    Ok(ToolOutput { files: vec![out.to_string_lossy().into_owned()], notes, ..ToolOutput::default() })
}
