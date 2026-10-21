//! Plugin mode from outside the reader's repository: this crate is built on
//! its own, installed into a host the way a user install would be, then
//! replaced while a document is open.

mod fixtures;

use std::path::Path;

use mireader_reader_host::{HostConfig, ReaderHost, sdk::Layout, stage_package};

const EXE: &str = env!("CARGO_BIN_EXE_mireader-reader-pdf");

#[test]
fn installs_opens_and_hot_swaps_from_a_standalone_build() {
    let dir = tempfile::tempdir().unwrap();
    // The package as this repository ships it: reader.toml + the executable.
    let built = dir.path().join("built");
    let package = stage_package(&Path::new(env!("CARGO_MANIFEST_DIR")).join("reader.toml"), Path::new(EXE), &built).unwrap();

    // A host that starts with no readers at all, like a fresh app data dir.
    let installed = dir.path().join("data").join("readers");
    let host = ReaderHost::new(HostConfig {
        dirs: vec![installed.clone()],
        install_dir: Some(installed.clone()),
        ..HostConfig::default()
    });
    assert!(host.reader_for("a.pdf").is_none());

    let reader = host.install_package(&package).unwrap();
    assert_eq!(reader.manifest.id, "mireader.pdf");
    assert!(installed.join("mireader.pdf").join("reader.toml").is_file());

    let file = dir.path().join("two-pages.pdf");
    std::fs::write(&file, fixtures::build()).unwrap();
    let doc = host.open(&file).unwrap();
    assert_eq!(doc.info().layout, Layout::Paged);
    assert_eq!(doc.render_page(0, 1.0).unwrap().width, 200);
    let first = host.running();
    assert_eq!(first.len(), 1, "{first:?}");

    // Reinstall while the document is open: the old process is stopped (its
    // exe is locked on Windows), the package replaced, and the open document
    // carries on in a new process.
    host.install_package(&package).unwrap();
    assert_eq!(doc.render_page(1, 1.0).unwrap().width, 300);
    let page = doc.render_page(0, 1.5).unwrap();
    let blue = fixtures::pixel(&page.png, 150, 75);
    assert!(blue[2] > 200 && blue[0] < 60, "the swapped-in process renders content: {blue:?}");
    let second = host.running();
    assert_eq!(second.len(), 1, "{second:?}");
    assert!(second[0].1 > first[0].1, "a new generation after the swap: {first:?} → {second:?}");

    // Uninstalled: nothing claims PDFs any more.
    host.uninstall("mireader.pdf").unwrap();
    assert!(host.reader_for("a.pdf").is_none());
    host.shutdown_all();
}
