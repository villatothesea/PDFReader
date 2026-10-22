//! End to end: the real PDF plugin executable driven by the real host.

mod fixtures;

use std::path::Path;

use mireader_reader_host::{HostConfig, HostError, ReaderHost, sdk::{Layout, Paper}, stage_package};

const EXE: &str = env!("CARGO_BIN_EXE_mireader-reader-pdf");

#[test]
fn renders_pages_through_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let readers = dir.path().join("readers");
    stage_package(&Path::new(env!("CARGO_MANIFEST_DIR")).join("reader.toml"), Path::new(EXE), &readers).unwrap();
    let host = ReaderHost::new(HostConfig {
        dirs: vec![readers],
        ..HostConfig::default()
    });
    let file = dir.path().join("two-pages.pdf");
    std::fs::write(&file, fixtures::build()).unwrap();

    let doc = host.open(&file).unwrap();
    let info = doc.info();
    assert_eq!(info.layout, Layout::Paged);
    assert_eq!(info.pages.len(), 2);
    assert_eq!(info.title.as_deref(), Some("Fixture Title"));

    let first = doc.render_page(0, 1.5, Paper::Document).unwrap();
    assert_eq!((first.width, first.height), (300, 150));
    let blue = fixtures::pixel(&first.png, 150, 75);
    assert!(blue[2] > 200 && blue[0] < 60, "{blue:?}");
    let second = doc.render_page(1, 1.0, Paper::Document).unwrap();
    assert_eq!((second.width, second.height), (300, 150));

    // Out-of-range pages are a request error; the process keeps serving.
    assert!(matches!(doc.render_page(7, 1.0, Paper::Document), Err(HostError::Reader(_))));
    assert!(doc.render_page(0, 0.5, Paper::Document).is_ok());

    // Not a PDF at all.
    let bad = dir.path().join("bad.pdf");
    std::fs::write(&bad, b"hello").unwrap();
    assert!(matches!(host.open(&bad), Err(HostError::Reader(_))));
    host.shutdown_all();
}

// MIREADER (round 11, item 18): the outline and the text layer reach the
// host through the real process.
#[test]
fn outline_and_page_text_cross_the_process_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let readers = dir.path().join("readers");
    stage_package(&Path::new(env!("CARGO_MANIFEST_DIR")).join("reader.toml"), Path::new(EXE), &readers).unwrap();
    let host = ReaderHost::new(HostConfig {
        dirs: vec![readers],
        ..HostConfig::default()
    });
    // Prewarm (round 11, #5): the reader starts before any open; formats
    // nothing claims are left alone.
    assert_eq!(host.prewarm("x.txt"), Ok(false));
    assert!(host.running().is_empty());
    assert_eq!(host.prewarm("D:/books/A.PDF"), Ok(true));
    let warmed = host.running();
    assert_eq!(warmed.len(), 1, "{warmed:?}");
    let file = dir.path().join("rich.pdf");
    std::fs::write(&file, fixtures::build_rich()).unwrap();
    let doc = host.open(&file).unwrap();
    assert_eq!(host.running(), warmed, "the open reuses the warm process");
    let titles: Vec<&str> = doc.info().outline.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, ["Intro", "Chapter 2"]);
    let text = doc.page_text(0).unwrap();
    assert_eq!(text.text, "Hello World\nSecond line");
    assert_eq!(text.boxes.len(), 23);
    assert_eq!(doc.text(0).unwrap(), text.text);
    let themed = doc
        .render_page(
            0,
            1.0,
            Paper::Theme {
                background: [30, 30, 30],
                ink: [230, 230, 230],
            },
        )
        .unwrap();
    assert_eq!(fixtures::pixel(&themed.png, 2, 2)[3], 0, "transparent ground");
    host.shutdown_all();
}
/// Timings for the Secretary's "first open feels slow" question (round 11,
/// #5): `MIREADER_TIMING_PDF=<file> cargo test --release -- --ignored --nocapture`.
#[test]
#[ignore = "prints timings for a real file"]
fn first_and_second_open_timings() {
    use std::time::Instant;
    let Some(file) = std::env::var_os("MIREADER_TIMING_PDF") else {
        return;
    };
    let file = std::path::PathBuf::from(file);
    let dir = tempfile::tempdir().unwrap();
    // MIREADER_TIMING_READERS: time an installed copy (e.g. a release's
    // readers\ folder) instead of a freshly staged one.
    let readers = match std::env::var_os("MIREADER_TIMING_READERS") {
        Some(installed) => std::path::PathBuf::from(installed),
        None => {
            let readers = dir.path().join("readers");
            stage_package(&Path::new(env!("CARGO_MANIFEST_DIR")).join("reader.toml"), Path::new(EXE), &readers).unwrap();
            readers
        }
    };
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    let decode = |png: &[u8]| {
        let t = Instant::now();
        let _ = fixtures::pixels(png);
        ms(t)
    };
    // The host's page scale for a ~900 px column at 150 % Windows scaling.
    let scale = 2.2;
    for round in ["cold (new process)", "warm process, second open"] {
        let host = ReaderHost::new(HostConfig {
            dirs: vec![readers.clone()],
            ..HostConfig::default()
        });
        if round.starts_with("warm") {
            let t = Instant::now();
            host.prewarm("x.pdf").unwrap();
            println!("  prewarm (spawn + hello): {:.0} ms", ms(t));
            let first = host.open(&file).unwrap();
            drop(first);
        }
        let t = Instant::now();
        let doc = host.open(&file).unwrap();
        let open = ms(t);
        let t = Instant::now();
        let page = doc.render_page(0, scale, Paper::Document).unwrap();
        let render = ms(t);
        let png_kb = page.png.len() as f64 / 1024.0;
        let decode = decode(&page.png);
        let t = Instant::now();
        // An installed copy from an older release has no text layer.
        let text = if std::env::var_os("MIREADER_TIMING_READERS").is_some() {
            Default::default()
        } else {
            doc.page_text(0).unwrap()
        };
        let text_ms = ms(t);
        let t = Instant::now();
        let _ = doc.render_page(1.min(doc.info().pages.len() as u32 - 1), scale, Paper::Document).unwrap();
        let next = ms(t);
        println!(
            "{round}: open {open:.0} ms, page 1 {render:.0} ms ({}×{} px, {png_kb:.0} KB PNG, decode {decode:.0} ms), \
             text layer {text_ms:.0} ms ({} chars), next page {next:.0} ms; {} pages, {} outline entries",
            page.width,
            page.height,
            text.text.chars().count(),
            doc.info().pages.len(),
            doc.info().outline.len()
        );
        host.shutdown_all();
    }
}

// Item 19: edits, undo, a save and a password cross the process boundary.
#[test]
fn edits_save_and_passwords_cross_the_process_boundary() {
    use mireader_reader_host::sdk::{EditOp, ErrorCode, MarkupKind};
    let dir = tempfile::tempdir().unwrap();
    let readers = dir.path().join("readers");
    stage_package(&Path::new(env!("CARGO_MANIFEST_DIR")).join("reader.toml"), Path::new(EXE), &readers).unwrap();
    let host = ReaderHost::new(HostConfig {
        dirs: vec![readers],
        ..HostConfig::default()
    });
    let original = fixtures::build_rich();
    let file = dir.path().join("rich.pdf");
    std::fs::write(&file, &original).unwrap();
    let doc = host.open(&file).unwrap();
    assert!(doc.info().edit.is_some(), "the PDF reader offers editing");
    let (state, id) = doc
        .edit(EditOp::AddMarkup {
            page: 0,
            kind: MarkupKind::Highlight,
            quads: vec![[20.0, 30.0, 120.0, 30.0, 20.0, 52.0, 120.0, 52.0]],
            color: None,
            contents: None,
        })
        .unwrap();
    assert!(state.dirty && id.is_some());
    assert_eq!(doc.annotations(0).unwrap().len(), 1);
    assert!(!doc.undo().unwrap().can_undo);
    assert!(doc.annotations(0).unwrap().is_empty());
    assert!(doc.redo().unwrap().dirty);
    doc.edit(EditOp::AddBookmark { parent: vec![], index: 0, title: "Added".into(), page: 0 }).unwrap();
    assert_eq!(doc.current_info().unwrap().outline[0].title, "Added");
    assert!(!doc.save().unwrap().dirty);
    assert!(std::fs::read(&file).unwrap().starts_with(&original));
    let copy = dir.path().join("copy.pdf");
    doc.save_as(&copy).unwrap();
    let again = host.open(&copy).unwrap();
    assert_eq!(again.annotations(0).unwrap().len(), 1);

    // A password-protected file: refused without its password, opened and
    // edited with it.
    let mut cos = pdfcraft_cos::Document::open(std::sync::Arc::new(original.clone())).unwrap();
    cos.set_encryption(&pdfcraft_cos::NewEncryption {
        algorithm: pdfcraft_cos::Algorithm::Aes256,
        user_password: "pw",
        owner_password: "owner",
        permissions: -1,
        encrypt_metadata: true,
        seed: [7; 32],
    })
    .unwrap();
    let locked = dir.path().join("locked.pdf");
    std::fs::write(&locked, pdfcraft_cos::write_full(&cos, &pdfcraft_cos::SaveOptions::default()).unwrap()).unwrap();
    assert!(matches!(host.open(&locked), Err(HostError::Reader(e)) if e.code == ErrorCode::Encrypted));
    assert!(matches!(host.open_with_password(&locked, Some("nope")), Err(HostError::Reader(e)) if e.code == ErrorCode::Encrypted));
    let opened = host.open_with_password(&locked, Some("pw")).unwrap();
    assert!(!opened.info().pages.is_empty());
    opened.edit(EditOp::AddNote { page: 0, at: [10.0, 10.0], contents: "n".into(), color: None }).unwrap();
    opened.save().unwrap();
    assert_eq!(host.open_with_password(&locked, Some("pw")).unwrap().annotations(0).unwrap().len(), 1);
    host.shutdown_all();
}
