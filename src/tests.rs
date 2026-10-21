use mireader_reader_sdk::{Document, ErrorCode};

use super::{PdfDocument, clamp_scale, decode_text};

#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

#[test]
fn reads_page_sizes_and_metadata() {
    let doc = PdfDocument::new(fixtures::build()).unwrap();
    let info = doc.info();
    assert_eq!(info.pages.len(), 2);
    assert_eq!((info.pages[0].width, info.pages[0].height), (200.0, 100.0));
    assert_eq!((info.pages[1].width, info.pages[1].height), (300.0, 150.0));
    assert_eq!(info.title.as_deref(), Some("Fixture Title"), "UTF-16BE title");
    assert_eq!(info.author.as_deref(), Some("Pat Printer"));
}

#[test]
fn renders_pages_at_the_requested_scale() {
    let mut doc = PdfDocument::new(fixtures::build()).unwrap();
    let page = doc.render_page(0, 2.0).unwrap();
    assert_eq!((page.width, page.height), (400, 200));
    // PDF y grows upward: the rectangle spans y 20..80 pt → rows 40..160 px.
    let inside = fixtures::pixel(&page.png, 200, 100);
    assert!(inside[2] > 200 && inside[0] < 60 && inside[1] < 60, "blue fill: {inside:?}");
    let outside = fixtures::pixel(&page.png, 5, 5);
    assert!(outside[..3].iter().all(|c| *c > 240), "white background: {outside:?}");
    assert!(doc.render_page(2, 1.0).is_err());
}

#[test]
fn huge_requests_are_clamped() {
    assert_eq!(clamp_scale(200.0, 100.0, 2.0), 2.0);
    let scale = clamp_scale(2000.0, 2000.0, 50.0);
    assert!(2000.0 * scale <= 16_384.0 && (2000.0 * scale).powi(2) <= 12_000_001.0);
    assert_eq!(clamp_scale(100.0, 100.0, f32::NAN), 1.0);
}

#[test]
fn garbage_is_invalid() {
    let error = PdfDocument::new(b"%PDF-1.4 nonsense".to_vec()).err().unwrap();
    assert_eq!(error.code, ErrorCode::InvalidDocument);
}

#[test]
fn text_strings_decode() {
    assert_eq!(decode_text(&[0xFE, 0xFF, 0x00, 0x41, 0x4E, 0x2D]), "A中");
    assert_eq!(decode_text(b"Caf\xe9"), "Café");
}
