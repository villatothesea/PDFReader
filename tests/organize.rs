//! Round 17 (O6): page organising through the host, as the app runs it —
//! now the PDF reader's own tools (moved from PDFInspector, round 13 D3).
//! The package both reads and acts.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use mireader_reader_host::{HostConfig, HostError, ReaderHost, ToolJob, stage_package};
use mireader_reader_sdk::{ErrorCode, Reader as _};

const EXE: &str = env!("CARGO_BIN_EXE_mireader-reader-pdf");

fn host_with_reader() -> (tempfile::TempDir, ReaderHost) {
    let readers = tempfile::tempdir().unwrap();
    stage_package(&Path::new(env!("CARGO_MANIFEST_DIR")).join("reader.toml"), Path::new(EXE), readers.path()).unwrap();
    let host = ReaderHost::new(HostConfig { dirs: vec![readers.path().to_path_buf()], ..HostConfig::default() });
    (readers, host)
}

/// A PDF whose page `n` (0-based) is `100 + 10 n` points wide, so a page's
/// width says which one it is.
fn numbered(pages: usize) -> Vec<u8> {
    let mut doc = pdfcraft_cos::Document::new_empty();
    for n in 0..pages {
        pdfcraft_organize::insert_blank_page(&mut doc, n, 100.0 + 10.0 * n as f64, 200.0).unwrap();
    }
    pdfcraft_cos::write_full(&doc, &pdfcraft_cos::SaveOptions::default()).unwrap()
}

/// The original page numbers (1-based) of a written PDF, by width.
fn page_numbers(path: &Path) -> Vec<u32> {
    let doc = mireader_reader_pdf::PdfReader.open(path).unwrap();
    doc.info().pages.iter().map(|page| ((page.width - 100.0) / 10.0).round() as u32 + 1).collect()
}

struct Run {
    host: ReaderHost,
    _readers: tempfile::TempDir,
    dir: tempfile::TempDir,
}

impl Run {
    fn new() -> Self {
        let (readers, host) = host_with_reader();
        Self { host, _readers: readers, dir: tempfile::tempdir().unwrap() }
    }

    fn pdf(&self, name: &str, pages: usize) -> std::path::PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, numbered(pages)).unwrap();
        path
    }

    /// Run `action` with `params` on `inputs`: the files written (by name)
    /// and the progress notes, or the error.
    fn job(
        &self,
        action: &str,
        inputs: &[&Path],
        params: serde_json::Value,
        cancel: bool,
    ) -> Result<(Vec<(String, Vec<u32>)>, Vec<String>), HostError> {
        let out = tempfile::tempdir_in(self.dir.path()).unwrap();
        let job = ToolJob {
            inputs: inputs.iter().map(|p| p.to_path_buf()).collect(),
            params: params.as_object().cloned().unwrap_or_default(),
            out_dir: Some(out.path().to_path_buf()),
        };
        let mut notes = Vec::new();
        let output = self.host.run_tool_job(
            "mireader.pdf",
            action,
            &job,
            &mut |p| notes.push(p.note.unwrap_or_default()),
            &AtomicBool::new(cancel),
        )?;
        let files = output
            .files
            .iter()
            .map(|file| {
                let path = Path::new(file);
                assert_eq!(path.parent().unwrap(), out.path(), "written where it was told");
                (path.file_name().unwrap().to_string_lossy().into_owned(), page_numbers(path))
            })
            .collect();
        Ok((files, notes))
    }
}

/// The reader opens PDFs and offers the page operations — one package.
#[test]
fn the_reader_reads_and_offers_its_page_tools() {
    let (_readers, host) = host_with_reader();
    let (installed, rejected) = host.readers();
    assert!(rejected.is_empty(), "{rejected:?}");
    assert!(installed[0].manifest.reads() && installed[0].manifest.acts());
    assert_eq!(host.reader_for("manual.pdf").unwrap().manifest.id, "mireader.pdf");
    let titles: Vec<String> = host.actions_for("D:/docs/Manual.PDF").into_iter().map(|a| a.action.title).collect();
    assert_eq!(
        titles,
        // MimiReader round 18: and Convert to Markdown, from the inspector;
        // round 19 (O12): plain text and images.
        [
            "Convert to Markdown",
            "Convert to plain text…",
            "Save pages as images…",
            "Extract pages…",
            "Reorder pages…",
            "Split every N pages…",
            "Split before pages…",
            "Merge with other PDFs…"
        ]
    );
}

#[test]
fn extract_and_reorder_pages() {
    let run = Run::new();
    let book = run.pdf("book.pdf", 5);
    let original = std::fs::read(&book).unwrap();
    let (files, _) = run.job("extract-pages", &[&book], serde_json::json!({ "pages": "2-3, 5" }), false).unwrap();
    assert_eq!(files, [("book (pages 2-3, 5).pdf".to_string(), vec![2, 3, 5])]);
    // Pages left out of the new order follow in their own.
    let (files, _) = run.job("reorder-pages", &[&book], serde_json::json!({ "order": "4, 1" }), false).unwrap();
    assert_eq!(files, [("book (reordered).pdf".to_string(), vec![4, 1, 2, 3, 5])]);
    let twice = run.job("reorder-pages", &[&book], serde_json::json!({ "order": "2, 1-3" }), false).unwrap_err();
    assert!(twice.to_string().contains("page 2 is listed twice"), "{twice}");
    let beyond = run.job("extract-pages", &[&book], serde_json::json!({ "pages": "4-9" }), false).unwrap_err();
    assert!(beyond.to_string().contains("no page 9"), "{beyond}");
    assert_eq!(std::fs::read(&book).unwrap(), original, "the original is never changed");
}

#[test]
fn split_every_n_and_before_pages_with_progress() {
    let run = Run::new();
    let book = run.pdf("book.pdf", 5);
    let (files, notes) = run.job("split-every", &[&book], serde_json::json!({ "every": 2 }), false).unwrap();
    assert_eq!(
        files,
        [
            ("book (pages 1-2).pdf".to_string(), vec![1, 2]),
            ("book (pages 3-4).pdf".to_string(), vec![3, 4]),
            ("book (pages 5).pdf".to_string(), vec![5]),
        ]
    );
    assert_eq!(notes, ["Part 1 of 3", "Part 2 of 3", "Part 3 of 3"]);
    let (files, _) = run.job("split-at", &[&book], serde_json::json!({ "at": "3, 5" }), false).unwrap();
    let pages: Vec<Vec<u32>> = files.into_iter().map(|(_, pages)| pages).collect();
    assert_eq!(pages, [vec![1, 2], vec![3, 4], vec![5]]);
}

#[test]
fn merge_in_the_order_given() {
    let run = Run::new();
    let book = run.pdf("book.pdf", 3);
    let more = run.pdf("more.pdf", 2);
    let (files, notes) = run.job("merge", &[&more, &book], serde_json::json!({}), false).unwrap();
    assert_eq!(files, [("more (merged).pdf".to_string(), vec![1, 2, 1, 2, 3])]);
    assert_eq!(notes, ["Reading more.pdf", "Reading book.pdf", "Merging"]);
}

#[test]
fn a_cancelled_job_writes_nothing() {
    let run = Run::new();
    let book = run.pdf("book.pdf", 4);
    let error = run.job("split-every", &[&book], serde_json::json!({ "every": 1 }), true).unwrap_err();
    assert_eq!(error, HostError::Cancelled);
    let broken = run.dir.path().join("broken.pdf");
    std::fs::write(&broken, "not a pdf").unwrap();
    let error = run.job("merge", &[&book, &broken], serde_json::json!({}), false).unwrap_err();
    assert!(matches!(&error, HostError::Reader(e) if e.code == ErrorCode::InvalidDocument && e.message.contains("broken.pdf")), "{error:?}");
}

/// Objects numbered from 1, a classic xref table and a trailer.
fn write_pdf(objects: &[&str]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (ix, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", ix + 1).into_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").into_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).into_bytes());
    out
}

/// Round 18 (#97): three pages (100, 110, 120 pt wide, as `numbered`) that
/// link to each other every way a PDF can: page 1 straight to page 3, page
/// 2 through the named destination `chap3` (a GoTo action), page 3 back to
/// page 1 at a height; and a bookmark "Two" to page 2 holding "Three", to a
/// point on page 3.
fn linked() -> Vec<u8> {
    write_pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [(chap3) [5 0 R /Fit]] >> >> /Outlines 9 0 R >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] /Annots [6 0 R] >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 110 200] /Annots [7 0 R] >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 200] /Annots [8 0 R] >>",
        "<< /Type /Annot /Subtype /Link /Rect [10 10 50 50] /Dest [5 0 R /Fit] >>",
        "<< /Type /Annot /Subtype /Link /Rect [10 10 50 50] /A << /S /GoTo /D (chap3) >> >>",
        "<< /Type /Annot /Subtype /Link /Rect [10 10 50 50] /Dest [3 0 R /XYZ 0 150 0] >>",
        "<< /Type /Outlines /First 10 0 R /Last 10 0 R /Count 2 >>",
        "<< /Title (Two) /Parent 9 0 R /Dest [4 0 R /Fit] /First 11 0 R /Last 11 0 R /Count 1 >>",
        "<< /Title (Three) /Parent 10 0 R /Dest [5 0 R /XYZ 0 100 0] >>",
    ])
}

/// Each page's original number (by width) and where its links go — the
/// target's index in this file and the original number of the page there.
fn links_of(path: &Path) -> Vec<(u32, Vec<(u32, u32)>)> {
    let mut doc = mireader_reader_pdf::PdfReader.open(path).unwrap();
    let info = doc.info();
    let original = |ix: u32| ((info.pages[ix as usize].width - 100.0) / 10.0).round() as u32 + 1;
    (0..info.pages.len() as u32)
        .map(|ix| {
            let targets = doc
                .page_text(ix)
                .unwrap()
                .links
                .iter()
                .filter_map(|link| match link.target {
                    mireader_reader_sdk::LinkTarget::Page { page, .. } => Some((page, original(page))),
                    _ => None,
                })
                .collect();
            (original(ix), targets)
        })
        .collect()
}

/// The bookmarks, depth first: their depth (0 = top), title and the
/// original number of the page each opens.
fn outline_of(path: &Path) -> Vec<(usize, String, u32)> {
    fn walk(entries: &[mireader_reader_sdk::OutlineEntry], depth: usize, widths: &[f32], out: &mut Vec<(usize, String, u32)>) {
        for entry in entries {
            if let mireader_reader_sdk::Target::Page(page) = entry.target {
                out.push((depth, entry.title.clone(), ((widths[page as usize] - 100.0) / 10.0).round() as u32 + 1));
            }
            walk(&entry.children, depth + 1, widths, out);
        }
    }
    let doc = mireader_reader_pdf::PdfReader.open(path).unwrap();
    let info = doc.info();
    let widths: Vec<f32> = info.pages.iter().map(|p| p.width).collect();
    let mut out = Vec::new();
    walk(&info.outline, 0, &widths, &mut out);
    out
}

fn mark(depth: usize, title: &str, page: u32) -> (usize, String, u32) {
    (depth, title.to_string(), page)
}

impl Run {
    /// A job on any PDF: the files it wrote (pages unread — a real
    /// document's pages don't say their number by width).
    fn job_any(&self, action: &str, inputs: &[&Path], params: serde_json::Value) -> Result<(Vec<String>, ()), HostError> {
        let out = tempfile::tempdir_in(self.dir.path()).unwrap().keep();
        let job = ToolJob {
            inputs: inputs.iter().map(|p| p.to_path_buf()).collect(),
            params: params.as_object().cloned().unwrap_or_default(),
            out_dir: Some(out),
        };
        Ok((self.host.run_tool_job("mireader.pdf", action, &job, &mut |_| {}, &AtomicBool::new(false))?.files, ()))
    }

    fn linked(&self, name: &str) -> std::path::PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, linked()).unwrap();
        path
    }

    /// The one file a job wrote.
    fn one(&self, action: &str, inputs: &[&Path], params: serde_json::Value) -> std::path::PathBuf {
        let out = tempfile::tempdir_in(self.dir.path()).unwrap().keep();
        let job = ToolJob {
            inputs: inputs.iter().map(|p| p.to_path_buf()).collect(),
            params: params.as_object().cloned().unwrap_or_default(),
            out_dir: Some(out),
        };
        let output = self.host.run_tool_job("mireader.pdf", action, &job, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        std::path::PathBuf::from(&output.files[0])
    }
}

/// Round 18 (#97): links and named destinations follow the pages they point
/// at through every operation — to the copy of the page, wherever it now
/// sits; a link to a page left out is dropped, never left pointing at the
/// wrong page. Bookmarks too.
#[test]
fn links_and_named_destinations_follow_their_pages() {
    let run = Run::new();
    let book = run.linked("book.pdf");
    assert_eq!(links_of(&book), [(1, vec![(2, 3)]), (2, vec![(2, 3)]), (3, vec![(0, 1)])], "the source, as made");
    assert_eq!(outline_of(&book), [mark(0, "Two", 2), mark(1, "Three", 3)]);

    // Reorder 3, 1, 2: every link still reaches its own page, at its new place.
    let reordered = run.one("reorder-pages", &[&book], serde_json::json!({ "order": "3, 1, 2" }));
    assert_eq!(
        links_of(&reordered),
        [(3, vec![(1, 1)]), (1, vec![(0, 3)]), (2, vec![(0, 3)])],
        "page 3 now first: 1→3 and the named chap3 point at index 0"
    );
    assert_eq!(outline_of(&reordered), [mark(0, "Two", 2), mark(1, "Three", 3)], "the bookmarks follow their pages");

    // Extract 1 and 3: their links meet, and "Three" comes along — its
    // parent's page didn't, so it moves up. Extract 2 alone: its link's page
    // is gone, its own bookmark stays.
    let ends = run.one("extract-pages", &[&book], serde_json::json!({ "pages": "1, 3" }));
    assert_eq!(links_of(&ends), [(1, vec![(1, 3)]), (3, vec![(0, 1)])]);
    assert_eq!(outline_of(&ends), [mark(0, "Three", 3)]);
    let middle = run.one("extract-pages", &[&book], serde_json::json!({ "pages": "2" }));
    assert_eq!(links_of(&middle), [(2, vec![])], "dropped, not pointing at the wrong page");
    assert_eq!(outline_of(&middle), [mark(0, "Two", 2)]);
    let pair = run.one("extract-pages", &[&book], serde_json::json!({ "pages": "3, 2" }));
    assert_eq!(outline_of(&pair), [mark(0, "Two", 2), mark(1, "Three", 3)], "nested as it was, in the new order");

    // Split before page 3: each part keeps the bookmarks of its own pages.
    let out = tempfile::tempdir_in(run.dir.path()).unwrap();
    let mut params = serde_json::Map::new();
    params.insert("at".into(), "3".into());
    let job = ToolJob { inputs: vec![book.clone()], params, out_dir: Some(out.path().to_path_buf()) };
    let parts = run.host.run_tool_job("mireader.pdf", "split-at", &job, &mut |_| {}, &AtomicBool::new(false)).unwrap().files;
    assert_eq!(outline_of(Path::new(&parts[0])), [mark(0, "Two", 2)]);
    assert_eq!(outline_of(Path::new(&parts[1])), [mark(0, "Three", 3)]);

    // Merge two copies: the second's links stay inside the second.
    let copy = run.linked("copy.pdf");
    let merged = run.one("merge", &[&book, &copy], serde_json::json!({}));
    assert_eq!(
        links_of(&merged),
        [(1, vec![(2, 3)]), (2, vec![(2, 3)]), (3, vec![(0, 1)]), (1, vec![(5, 3)]), (2, vec![(5, 3)]), (3, vec![(3, 1)])]
    );
}

/// Real manuals, for timings and a look at the parts:
/// `MIREADER_TEST_PDF=<a.pdf> MIREADER_TEST_PDF2=<b.pdf>`.
#[test]
#[ignore = "needs two sample PDFs"]
fn organise_real_pdfs() {
    let run = Run::new();
    let a = std::path::PathBuf::from(std::env::var("MIREADER_TEST_PDF").expect("MIREADER_TEST_PDF"));
    let b = std::path::PathBuf::from(std::env::var("MIREADER_TEST_PDF2").expect("MIREADER_TEST_PDF2"));
    let pages = |p: &Path| mireader_reader_pdf::PdfReader.open(p).unwrap().info().pages.len();
    let started = std::time::Instant::now();
    let (files, _) = run.job("split-every", &[&a], serde_json::json!({ "every": 10 }), false).unwrap();
    let parts: usize = files.iter().map(|(_, p)| p.len()).sum();
    eprintln!("split {} ({} pages) into {} parts, {parts} pages, in {:?}", a.display(), pages(&a), files.len(), started.elapsed());
    assert_eq!(parts, pages(&a));
    let started = std::time::Instant::now();
    let (files, _) = run.job("merge", &[&a, &b], serde_json::json!({}), false).unwrap();
    eprintln!("merged into {} pages in {:?}", files[0].1.len(), started.elapsed());
    assert_eq!(files[0].1.len(), pages(&a) + pages(&b));
    // Round 18 (#97): a reorder permutes the document's own pages, one move
    // each — the worst case is a full reversal.
    let n = pages(&a);
    let reversed = format!("{n}-1");
    let started = std::time::Instant::now();
    let (files, _) = run.job_any("reorder-pages", &[&a], serde_json::json!({ "order": reversed })).unwrap();
    eprintln!("reversed {n} pages in {:?}", started.elapsed());
    assert_eq!(files.len(), 1);
}
