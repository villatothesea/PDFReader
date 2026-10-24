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
        ["Extract pages…", "Reorder pages…", "Split every N pages…", "Split before pages…", "Merge with other PDFs…"]
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
}
