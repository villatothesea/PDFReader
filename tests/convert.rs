//! MimiReader round 18: Convert to Markdown as the app runs it — one of the
//! PDF reader's own tools now (it was the retired PDFInspector's), through
//! the host's catalog and the package's job process.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use mireader_reader_host::{HostConfig, HostError, ReaderHost, ToolJob, stage_package};

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

/// `pages` A4-ish pages; the first a 24 pt title, two 12 pt paragraph
/// lines, a gap, a second paragraph and two bullet items (Helvetica); the
/// rest a line of text each.
fn manual(pages: usize) -> Vec<u8> {
    let first = b"BT /F1 24 Tf 72 760 Td (Kit Manual) Tj ET\n\
BT /F1 12 Tf 72 720 Td (The kit was recorded in Studio Two with) Tj 0 -14.4 Td (sixteen microphones.) Tj ET\n\
BT /F1 12 Tf 72 670 Td (Every drum has its own channel.) Tj ET\n\
BT /F1 12 Tf 72 640 Td (- Snare) Tj 0 -14.4 Td (- Kick) Tj ET\n"
        .to_vec();
    let kids: Vec<String> = (0..pages).map(|n| format!("{} 0 R", 4 + 2 * n)).collect();
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    for n in 0..pages {
        let content = if n == 0 { first.clone() } else { format!("BT /F1 12 Tf 72 720 Td (Page {} of the manual.) Tj ET\n", n + 1).into_bytes() };
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
                5 + 2 * n
            )
            .into_bytes(),
        );
        objects.push(stream(&content));
    }
    write_pdf(&objects)
}

fn host_with_reader() -> (tempfile::TempDir, ReaderHost) {
    let readers = tempfile::tempdir().unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("reader.toml");
    stage_package(&manifest, Path::new(env!("CARGO_BIN_EXE_mireader-reader-pdf")), readers.path()).unwrap();
    let host = ReaderHost::new(HostConfig { dirs: vec![readers.path().to_path_buf()], ..HostConfig::default() });
    (readers, host)
}

#[test]
fn the_reader_converts_a_pdf_to_markdown() {
    let (_readers, host) = host_with_reader();
    let actions = host.actions_for("D:/docs/Manual.PDF");
    let convert = actions.iter().find(|a| a.action.title == "Convert to Markdown").expect("offered for a PDF");
    assert_eq!((convert.tool_id.as_str(), convert.action.id.as_str()), ("mireader.pdf", "pdf-to-markdown"));
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("manual.pdf");
    std::fs::write(&pdf, manual(1)).unwrap();
    let output = host.run_tool(&convert.tool_id, &convert.action.id, &pdf).unwrap();
    assert_eq!(output.mime, "text/markdown");
    assert_eq!(
        output.text,
        "# Kit Manual\n\n\
         The kit was recorded in Studio Two with sixteen microphones.\n\n\
         Every drum has its own channel.\n\n\
         - Snare\n\
         - Kick\n"
    );
    assert!(host.running().is_empty(), "it ran in the job process, not one for documents: {:?}", host.running());
    assert_eq!(host.running_jobs().len(), 1);
}

/// It says how far it has got, page by page, and stops when asked — the
/// inspector did neither, so a cancel always ended in a kill.
#[test]
fn a_conversion_reports_each_page_and_stops_when_asked() {
    let (_readers, host) = host_with_reader();
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("long.pdf");
    std::fs::write(&pdf, manual(6)).unwrap();
    let job = ToolJob { inputs: vec![pdf.clone()], ..ToolJob::default() };
    let mut seen = Vec::new();
    host.run_tool_job("mireader.pdf", "pdf-to-markdown", &job, &mut |p| seen.push((p.done, p.total)), &AtomicBool::new(false)).unwrap();
    assert_eq!(seen, (0..6).map(|n| (n, 6)).collect::<Vec<_>>());
    let generation = host.running_jobs()[0].1;
    let cancel = AtomicBool::new(false);
    let result = host.run_tool_job(
        "mireader.pdf",
        "pdf-to-markdown",
        &job,
        &mut |p| {
            if p.done == 2 {
                cancel.store(true, Ordering::Release);
            }
        },
        &cancel,
    );
    assert_eq!(result.unwrap_err(), HostError::Cancelled);
    assert_eq!(host.running_jobs()[0].1, generation, "it stopped by itself: no kill, no new process");
}

#[test]
fn a_pdf_without_text_is_refused_with_a_reason() {
    let (_readers, host) = host_with_reader();
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("scan.pdf");
    std::fs::write(
        &pdf,
        write_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>".to_vec(),
            stream(b"0 0 1 rg 20 20 160 60 re f\n"),
        ]),
    )
    .unwrap();
    let error = host.run_tool("mireader.pdf", "pdf-to-markdown", &pdf).unwrap_err();
    assert!(error.to_string().contains("no text layer"), "{error}");
}

/// A real manual, for eyeballing the output:
/// `MIREADER_TEST_PDF=<file.pdf> MIREADER_TEST_OUT=<file.md>`.
#[test]
#[ignore = "needs a sample PDF"]
fn convert_a_real_pdf() {
    let pdf = std::env::var("MIREADER_TEST_PDF").expect("MIREADER_TEST_PDF");
    let out = std::env::var("MIREADER_TEST_OUT").expect("MIREADER_TEST_OUT");
    let started = std::time::Instant::now();
    let markdown = mireader_reader_pdf::convert::pdf_to_markdown(Path::new(&pdf)).unwrap();
    eprintln!("{} chars in {:?}", markdown.len(), started.elapsed());
    std::fs::write(out, markdown).unwrap();
}
