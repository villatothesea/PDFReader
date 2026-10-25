//! MimiReader round 19 (O12, T1 item 2): pages as images and the text
//! layer as plain text, as the app runs them — through the host's catalog
//! and the package's job process.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mireader_reader_host::{HostConfig, HostError, ReaderHost, ToolJob, stage_package};
use mireader_reader_sdk::ToolOutput;

/// Objects numbered from 1, a classic xref table and a trailer.
fn write_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (ix, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", ix + 1).into_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").into_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).into_bytes());
    out
}

fn stream(data: &[u8]) -> Vec<u8> {
    [format!("<< /Length {} >>\nstream\n", data.len()).into_bytes(), data.to_vec(), b"\nendstream".to_vec()].concat()
}

/// One page: its size in points and its content stream.
struct Page {
    width: f32,
    height: f32,
    content: String,
}

/// An A4 page with two lines of Helvetica text.
fn text_page(first: &str, second: &str) -> Page {
    Page {
        width: 595.0,
        height: 842.0,
        content: format!("BT /F1 12 Tf 72 760 Td ({first}) Tj 0 -14.4 Td ({second}) Tj ET\n"),
    }
}

/// An A4 page with only a filled rectangle: a scan, as far as text goes.
fn picture_page() -> Page {
    Page { width: 595.0, height: 842.0, content: "0 0 1 rg 72 400 300 200 re f\n".into() }
}

fn pdf(pages: &[Page]) -> Vec<u8> {
    let kids: Vec<String> = (0..pages.len()).map(|n| format!("{} 0 R", 4 + 2 * n)).collect();
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    for (n, page) in pages.iter().enumerate() {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
                page.width,
                page.height,
                5 + 2 * n
            )
            .into_bytes(),
        );
        objects.push(stream(page.content.as_bytes()));
    }
    write_pdf(&objects)
}

struct Run {
    host: ReaderHost,
    _readers: tempfile::TempDir,
    dir: tempfile::TempDir,
}

impl Run {
    fn new() -> Self {
        let readers = tempfile::tempdir().unwrap();
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("reader.toml");
        stage_package(&manifest, Path::new(env!("CARGO_BIN_EXE_mireader-reader-pdf")), readers.path()).unwrap();
        let host = ReaderHost::new(HostConfig { dirs: vec![readers.path().to_path_buf()], ..HostConfig::default() });
        Self { host, _readers: readers, dir: tempfile::tempdir().unwrap() }
    }

    fn pdf(&self, name: &str, pages: &[Page]) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, pdf(pages)).unwrap();
        path
    }

    /// Run `action` on `input` with `params`: its output and progress notes.
    fn job(&self, action: &str, input: &Path, params: serde_json::Value) -> Result<(ToolOutput, Vec<String>), HostError> {
        self.job_with(action, input, params, &mut |_| {}, &AtomicBool::new(false))
    }

    fn job_with(
        &self,
        action: &str,
        input: &Path,
        params: serde_json::Value,
        each: &mut dyn FnMut(u64),
        cancel: &AtomicBool,
    ) -> Result<(ToolOutput, Vec<String>), HostError> {
        let out = tempfile::tempdir_in(self.dir.path()).unwrap().keep();
        let job = ToolJob {
            inputs: vec![input.to_path_buf()],
            params: params.as_object().cloned().unwrap_or_default(),
            out_dir: Some(out),
        };
        let mut notes = Vec::new();
        let output = self.host.run_tool_job(
            "mireader.pdf",
            action,
            &job,
            &mut |p| {
                each(p.done);
                notes.push(p.note.unwrap_or_default());
            },
            cancel,
        )?;
        Ok((output, notes))
    }
}

/// A PNG's size, from its header.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[1..4], b"PNG", "{} is a PNG", path.display());
    let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

fn names(output: &ToolOutput) -> Vec<String> {
    output.files.iter().map(|f| Path::new(f).file_name().unwrap().to_string_lossy().into_owned()).collect()
}

/// Both offered for a PDF, after Convert to Markdown, with the forms the
/// app draws: pages (empty: every page), and the resolution.
#[test]
fn both_are_offered_with_their_forms() {
    let run = Run::new();
    let actions = run.host.actions_for("D:/docs/Paper.pdf");
    let text = actions.iter().find(|a| a.action.id == "pdf-to-text").expect("Convert to plain text");
    assert_eq!(text.action.title, "Convert to plain text…");
    assert_eq!(text.action.output.as_deref(), Some("txt"));
    let images = actions.iter().find(|a| a.action.id == "pdf-to-images").expect("Save pages as images");
    assert_eq!(images.action.files.as_deref(), Some("png"));
    let dpi = images.action.params.iter().find(|p| p.id == "dpi").expect("a resolution");
    assert_eq!((format!("{:?}", dpi.default), dpi.min, dpi.max), ("Some(Integer(150))".to_string(), Some(72), Some(300)));
    for action in [text, images] {
        let pages = action.action.params.iter().find(|p| p.id == "pages").expect("pages");
        assert!(!pages.required, "{}: empty means every page", action.action.id);
    }
}

/// Page by page in order, a form feed between pages — a page without text
/// keeps its place (page n is after n − 1 form feeds) and is named in a
/// note; the pages asked for, in the order asked.
#[test]
fn plain_text_is_the_pages_in_order_with_a_form_feed_between() {
    let run = Run::new();
    let paper = run.pdf(
        "paper.pdf",
        &[text_page("Title of the paper", "by Someone"), picture_page(), text_page("Results", "were good.")],
    );
    let (output, progress) = run.job("pdf-to-text", &paper, serde_json::json!({})).unwrap();
    assert_eq!(output.mime, "text/plain");
    assert_eq!(output.text, "Title of the paper\nby Someone\n\u{c}\u{c}Results\nwere good.\n");
    assert_eq!(output.notes, ["no text on page 2 (scans or images)"]);
    assert_eq!(progress, ["Page 1 of 3", "Page 2 of 3", "Page 3 of 3"]);
    let (output, _) = run.job("pdf-to-text", &paper, serde_json::json!({ "pages": "3, 1" })).unwrap();
    assert_eq!(output.text, "Results\nwere good.\n\u{c}Title of the paper\nby Someone\n");
    assert!(output.notes.is_empty());
    assert!(run.host.running().is_empty(), "it ran in the job process, not one for documents");
}

/// A scan is refused with the reason — never an empty file.
#[test]
fn plain_text_of_a_scan_is_refused_not_empty() {
    let run = Run::new();
    let scan = run.pdf("scan.pdf", &[picture_page(), picture_page()]);
    let error = run.job("pdf-to-text", &scan, serde_json::json!({})).unwrap_err();
    assert!(error.to_string().contains("No text found: these pages have no text layer"), "{error}");
    let paper = run.pdf("paper.pdf", &[text_page("A", "B"), picture_page()]);
    let error = run.job("pdf-to-text", &paper, serde_json::json!({ "pages": "2" })).unwrap_err();
    assert!(error.to_string().contains("No text found: this page has no text layer"), "{error}");
}

/// One PNG a page, `{stem}-001.png` …, at 150 DPI unless asked; the
/// resolution held to 72–300; a page asked for twice written once.
#[test]
fn pages_become_pngs_named_to_sort_at_the_resolution_asked() {
    let run = Run::new();
    let paper = run.pdf("paper.pdf", &[text_page("One", "a"), text_page("Two", "b"), picture_page()]);
    let (output, progress) = run.job("pdf-to-images", &paper, serde_json::json!({})).unwrap();
    assert_eq!(names(&output), ["paper-001.png", "paper-002.png", "paper-003.png"]);
    assert_eq!(progress, ["Page 1 of 3", "Page 2 of 3", "Page 3 of 3"]);
    // A4 is 595 × 842 points: at 150 DPI, 1239.6 × 1754.2 pixels — whole
    // pixels, rounded down.
    assert_eq!(png_size(Path::new(&output.files[0])), (1239, 1754));
    assert!(output.notes.is_empty());
    let (output, _) = run.job("pdf-to-images", &paper, serde_json::json!({ "pages": "3, 1, 3", "dpi": 300 })).unwrap();
    assert_eq!(names(&output), ["paper-003.png", "paper-001.png"], "in the order asked, each once");
    assert_eq!(png_size(Path::new(&output.files[0])), (2479, 3508));
    let (output, _) = run.job("pdf-to-images", &paper, serde_json::json!({ "pages": "1", "dpi": 2400 })).unwrap();
    assert_eq!(png_size(Path::new(&output.files[0])), (2479, 3508), "held to 300 DPI");
    let (output, _) = run.job("pdf-to-images", &paper, serde_json::json!({ "pages": "1", "dpi": 10 })).unwrap();
    assert_eq!(png_size(Path::new(&output.files[0])), (595, 842), "held to 72 DPI");
}

/// A page too large for the renderer's ceiling at the resolution asked is
/// saved at the most it allows — and a note says at what.
#[test]
fn a_page_too_large_for_the_resolution_says_what_it_got() {
    let run = Run::new();
    // A0: 2384 × 3370 points, ~100 megapixels at 300 DPI.
    let poster = run.pdf("poster.pdf", &[Page { width: 2384.0, height: 3370.0, content: "0 0 1 rg 72 72 300 300 re f\n".into() }]);
    let (output, _) = run.job("pdf-to-images", &poster, serde_json::json!({ "dpi": 300 })).unwrap();
    let (width, height) = png_size(Path::new(&output.files[0]));
    assert!(width * height <= 12_000_000, "{width} × {height}");
    assert_eq!(output.notes.len(), 1, "{:?}", output.notes);
    assert!(output.notes[0].starts_with("page 1 is too large for 300 DPI: saved at "), "{:?}", output.notes);
}

/// Stopped mid-way, it stops: the next page isn't rendered.
#[test]
fn an_image_export_stops_when_asked() {
    let run = Run::new();
    let pages: Vec<Page> = (0..6).map(|n| text_page(&format!("Page {n}"), "text")).collect();
    let paper = run.pdf("long.pdf", &pages);
    let cancel = AtomicBool::new(false);
    let mut last = 0;
    let result = run.job_with(
        "pdf-to-images",
        &paper,
        serde_json::json!({}),
        &mut |done| {
            last = done;
            if done == 2 {
                cancel.store(true, Ordering::Release);
            }
        },
        &cancel,
    );
    assert_eq!(result.unwrap_err(), HostError::Cancelled);
    assert_eq!(last, 2, "no page after the cancel");
}

/// MIREADER_TEST_PDF: a real paper, timed — what a user waits for and what
/// lands on disk, for both. `cargo test --release -- --ignored --nocapture`.
#[test]
#[ignore = "needs MIREADER_TEST_PDF"]
fn a_real_pdf_timed() {
    let source = PathBuf::from(std::env::var("MIREADER_TEST_PDF").expect("MIREADER_TEST_PDF"));
    let run = Run::new();
    let started = std::time::Instant::now();
    let (text, _) = run.job("pdf-to-text", &source, serde_json::json!({})).unwrap();
    let text_time = started.elapsed();
    let pages = text.text.matches('\u{c}').count() + 1;
    eprintln!("text: {pages} pages, {} bytes, {:.2} s, notes {:?}", text.text.len(), text_time.as_secs_f64(), text.notes);
    for dpi in [150, 300] {
        let started = std::time::Instant::now();
        let (images, _) = run.job("pdf-to-images", &source, serde_json::json!({ "dpi": dpi })).unwrap();
        let elapsed = started.elapsed();
        let bytes: u64 = images.files.iter().map(|f| std::fs::metadata(f).unwrap().len()).sum();
        eprintln!(
            "images at {dpi} DPI: {} files, {:.1} MB, {:.2} s, notes {:?}",
            images.files.len(),
            bytes as f64 / 1_048_576.0,
            elapsed.as_secs_f64(),
            images.notes
        );
    }
}
