use mireader_reader_sdk::{Document, ErrorCode, Paper, Target};

use super::{PdfDocument, clamp_scale, decode_text, recolor, scan::Ground};

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

// MIREADER (round 11, item 18): bookmarks, the text layer, and paper that
// follows the theme.

const DARK: Paper = Paper::Theme {
    background: [30, 30, 30],
    ink: [230, 230, 230],
};

#[test]
fn outline_reads_explicit_nested_and_named_destinations() {
    let doc = PdfDocument::new(fixtures::build_rich()).unwrap();
    let outline = doc.info().outline;
    let titles: Vec<(&str, Target)> = outline.iter().map(|e| (e.title.as_str(), e.target)).collect();
    assert_eq!(titles, [("Intro", Target::Page(0)), ("Chapter 2", Target::Page(1))]);
    assert_eq!(outline[0].children.len(), 1);
    assert_eq!(outline[0].children[0].title, "Details");
    assert_eq!(outline[0].children[0].target, Target::Page(2));
    // No outline at all is an empty list, not an error.
    assert!(PdfDocument::new(fixtures::build()).unwrap().info().outline.is_empty());
}

#[test]
fn page_text_is_in_reading_order_with_one_box_per_char() {
    let mut doc = PdfDocument::new(fixtures::build_rich()).unwrap();
    let text = doc.page_text(0).unwrap();
    assert_eq!(text.text, "Hello World\nSecond line");
    assert_eq!(text.boxes.len(), text.text.chars().count());
    // "H" starts at x = 20 pt; its baseline is 50 pt from the top, and a
    // 20 pt em reaches from 16 pt above to 4 pt below it.
    let [x0, y0, x1, y1] = text.boxes[0];
    assert!((x0 - 20.0).abs() < 0.5, "{:?}", text.boxes[0]);
    assert!((y0 - 34.0).abs() < 0.5 && (y1 - 54.0).abs() < 0.5, "{:?}", text.boxes[0]);
    assert!(x1 > x0 + 10.0 && x1 < x0 + 20.0, "H is about 0.72 em wide: {:?}", text.boxes[0]);
    // Boxes advance along the line; the second line sits 30 pt lower.
    assert!(text.boxes[1][0] >= x1 - 0.01);
    let second = text.text.chars().position(|c| c == 'S').unwrap();
    assert!((text.boxes[second][1] - 64.0).abs() < 0.5, "{:?}", text.boxes[second]);
    // The inserted line break has an empty box.
    let newline = text.text.chars().position(|c| c == '\n').unwrap();
    assert_eq!(text.boxes[newline][0], text.boxes[newline][2]);
    // Search uses the same text.
    assert_eq!(doc.text(0).unwrap(), "Hello World\nSecond line");
    assert_eq!(doc.page_text(3).unwrap().text, "", "a blank page has no text");
    assert!(doc.page_text(9).is_err());
}

#[test]
fn pages_are_classified_by_their_ground() {
    let mut doc = PdfDocument::new(fixtures::build_rich()).unwrap();
    assert_eq!(doc.scan(0).unwrap().ground, Ground::Plain);
    assert!(matches!(doc.scan(1).unwrap().ground, Ground::Declared([255, 0, 0, 255])));
    assert_eq!(doc.scan(2).unwrap().ground, Ground::Image);
    assert_eq!(doc.scan(3).unwrap().ground, Ground::Plain);
}

/// The most opaque pixel inside a page-point box, at `scale`.
fn ink_in(png: &[u8], scale: f32, b: [f32; 4]) -> [u8; 4] {
    let (pixels, width) = fixtures::pixels(png);
    let mut best = [0u8; 4];
    for y in (b[1] * scale) as u32..(b[3] * scale) as u32 {
        for x in (b[0] * scale) as u32..(b[2] * scale) as u32 {
            let px = pixels[(y * width + x) as usize];
            if px[3] > best[3] || (px[3] == best[3] && px[..3] < best[..3]) {
                best = px;
            }
        }
    }
    best
}

#[test]
fn themed_paper_lightens_plain_pages_and_keeps_document_colours_elsewhere() {
    let mut doc = PdfDocument::new(fixtures::build_rich()).unwrap();
    let h = doc.page_text(0).unwrap().boxes[0];

    // As printed: white paper, black ink.
    let printed = doc.render_page_on(0, 2.0, Paper::Document).unwrap();
    assert!(fixtures::pixel(&printed.png, 4, 4)[..3].iter().all(|c| *c > 240));
    let ink = ink_in(&printed.png, 2.0, h);
    assert!(ink[3] == 255 && ink[..3].iter().all(|c| *c < 60), "black ink: {ink:?}");

    // On a dark theme: no paper at all, light ink.
    let themed = doc.render_page_on(0, 2.0, DARK).unwrap();
    assert_eq!(fixtures::pixel(&themed.png, 4, 4)[3], 0, "the theme shows through");
    let ink = ink_in(&themed.png, 2.0, h);
    assert!(ink[3] > 200 && ink[..3].iter().all(|c| *c > 180), "light ink: {ink:?}");

    // A declared ground keeps the document's colours.
    let red = doc.render_page_on(1, 1.0, DARK).unwrap();
    assert_eq!(fixtures::pixel(&red.png, 5, 5), [255, 0, 0, 255]);

    // An image page is left exactly as it is.
    let image = doc.render_page_on(2, 1.0, DARK).unwrap();
    let corner = fixtures::pixel(&image.png, 5, 5);
    assert!(corner[0] > 200 && corner[1] < 60 && corner[2] < 60 && corner[3] == 255, "{corner:?}");
}

#[test]
fn the_theme_mapping_sends_white_to_the_ground_and_black_to_the_ink() {
    let ground = recolor::to_oklab([30, 30, 30]);
    let ink = recolor::to_oklab([230, 230, 230]);
    let close = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2);
    assert!(close(recolor::remap_color([255, 255, 255], ground, ink), [30, 30, 30]));
    assert!(close(recolor::remap_color([0, 0, 0], ground, ink), [230, 230, 230]));
    // A coloured ink keeps its hue: red text stays reddish.
    let red = recolor::remap_color([200, 0, 0], ground, ink);
    assert!(red[0] > red[1] + 40 && red[0] > red[2] + 40, "{red:?}");
}