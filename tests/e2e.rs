//! End to end: the real PDF plugin executable driven by the real host.

mod fixtures;

use std::path::Path;

use mireader_reader_host::{HostConfig, HostError, ReaderHost, sdk::Layout, stage_package};

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

    let first = doc.render_page(0, 1.5).unwrap();
    assert_eq!((first.width, first.height), (300, 150));
    let blue = fixtures::pixel(&first.png, 150, 75);
    assert!(blue[2] > 200 && blue[0] < 60, "{blue:?}");
    let second = doc.render_page(1, 1.0).unwrap();
    assert_eq!((second.width, second.height), (300, 150));

    // Out-of-range pages are a request error; the process keeps serving.
    assert!(matches!(doc.render_page(7, 1.0), Err(HostError::Reader(_))));
    assert!(doc.render_page(0, 0.5).is_ok());

    // Not a PDF at all.
    let bad = dir.path().join("bad.pdf");
    std::fs::write(&bad, b"hello").unwrap();
    assert!(matches!(host.open(&bad), Err(HostError::Reader(_))));
    host.shutdown_all();
}
