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

/// Round 19 (the corpus sweep, noembed-sjis): Japanese text whose font isn't
/// embedded is drawn, in the system's Japanese font — it drew nothing while
/// its text could be selected. (Skips where the system has no such font.)
#[test]
fn non_embedded_japanese_is_drawn() {
    if !crate::fallback::has_font_for(&hayro::hayro_interpret::hayro_cmap::CidFamily::AdobeJapan1) {
        eprintln!("skipped: no Japanese system font here");
        return;
    }
    let mut doc = PdfDocument::new(fixtures::build_noembed_cjk()).unwrap();
    let page = doc.render_page(0, 2.0).unwrap();
    let (pixels, _) = fixtures::pixels(&page.png);
    let ink = pixels.iter().filter(|p| p[0] < 128 && p[1] < 128 && p[2] < 128).count();
    assert!(ink > 200, "the text is drawn: {ink} dark pixels");
    let text = doc.page_text(0).unwrap().text;
    assert!(text.contains("日本"), "and reads as itself: {text:?}");
}

/// Round 19 (the corpus sweep, issue1350): a page's text is what its crop
/// box shows. Two pages drawing the same content through different crop
/// boxes each give only their own half — the text layer gave every page
/// the whole content, so search found matches on pages that don't show them.
#[test]
fn a_pages_text_is_only_what_its_crop_box_shows() {
    let mut doc = PdfDocument::new(fixtures::build_cropped()).unwrap();
    let top = doc.page_text(0).unwrap().text;
    let bottom = doc.page_text(1).unwrap().text;
    assert!(top.contains("Top half") && !top.contains("Bottom half"), "page 1: {top:?}");
    assert!(bottom.contains("Bottom half") && !bottom.contains("Top half"), "page 2: {bottom:?}");
}

/// Round 19 (the corpus sweep, bug1650302 and six more pages): a ToUnicode
/// given as the name /Identity-H means each code is its own Unicode value.
/// hayro parsed it as the CID CMap of that name, which maps nothing to
/// text, so the page had no text layer at all.
#[test]
fn an_identity_to_unicode_name_gives_the_codes_as_text() {
    let mut doc = PdfDocument::new(fixtures::build_identity_to_unicode()).unwrap();
    let text = doc.page_text(0).unwrap().text;
    assert_eq!(text.trim(), "Hi");
}

/// Round 19 (the corpus sweep, text-layer group 5): a Type0 font with an
/// Identity ordering and no ToUnicode gave no text at all, though the font
/// it embeds says in its cmap which character each glyph is. The test
/// embeds the system's Arial (no font is committed) and skips without it.
#[test]
fn an_embedded_fonts_cmap_names_glyphs_that_have_no_to_unicode() {
    use skrifa::MetadataProvider as _;
    let Some(font) = system_arial() else { return };
    let charmap = skrifa::FontRef::new(&font).unwrap().charmap();
    let glyphs: Vec<u32> = "Hi".chars().map(|c| charmap.map(c).unwrap().to_u32()).collect();
    let mut doc = PdfDocument::new(fixtures::build_embedded_identity(&font, &glyphs)).unwrap();
    assert_eq!(doc.page_text(0).unwrap().text.trim(), "Hi");
}

/// Round 19 (the corpus sweep, vertical, issue13147 and three more pages):
/// an embedded CID font of a known collection (here Adobe-Japan1) with no
/// ToUnicode gave no text. hayro had the collection's CID → Unicode table
/// but used it only for a stand-in font (vendored patch 4). CIDs 41 and 74
/// are "H" and "i" in Adobe-Japan1; the fixture draws glyph 0 for both, so
/// nothing else can name them.
#[test]
fn an_embedded_cid_font_of_a_known_collection_has_text() {
    let Some(font) = system_arial() else { return };
    let mut doc = PdfDocument::new(fixtures::build_embedded_collection(&font, "Japan1", &[41, 74])).unwrap();
    assert_eq!(doc.page_text(0).unwrap().text.trim(), "Hi");
}

/// Round 19 (the corpus sweep, P2, 65 pages): a field or comment the file
/// leaves the viewer to draw (no `/AP`) was missing from the page and its
/// text — a filled-in form showed empty boxes. The pages are drawn from a
/// copy with those appearances added.
#[test]
fn fields_and_comments_without_appearances_are_drawn() {
    let mut doc = PdfDocument::new(fixtures::build_without_appearances()).unwrap();
    let text = doc.page_text(0).unwrap().text;
    assert!(text.contains("Hello"), "the field's value is on the page: {text:?}");

    let page = doc.render_page(0, 2.0).unwrap();
    assert_eq!((page.width, page.height), (400, 200));
    let (pixels, width) = fixtures::pixels(&page.png);
    // PDF y grows upward: a point (x, y) is pixel (2x, 2(100 - y)).
    let inked = |x0: f64, y0: f64, x1: f64, y1: f64| {
        let (cols, rows) = ((2.0 * x0) as u32..(2.0 * x1) as u32, (2.0 * (100.0 - y1)) as u32..(2.0 * (100.0 - y0)) as u32);
        rows.flat_map(|y| cols.clone().map(move |x| (x, y)))
            .filter(|&(x, y)| pixels[(y * width + x) as usize][..3].iter().any(|c| *c < 160))
            .count()
    };
    assert!(inked(20.0, 60.0, 180.0, 90.0) > 40, "the text field's value is drawn");
    assert!(inked(22.0, 22.0, 38.0, 38.0) > 20, "the checked box shows its mark");
    let red = fixtures::pixel(&page.png, 241, 140);
    assert!(red[0] > 180 && red[1] < 90 && red[2] < 90, "the square comment's red border: {red:?}");
}

/// Round 20 (#175): an attached file is listed with its size and date,
/// and written out decoded.
#[test]
fn an_attachment_is_listed_and_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("layered.pdf");
    std::fs::write(&path, fixtures::build_layer_and_attachment()).unwrap();
    let mut doc = mireader_reader_sdk::Reader::open(&mut crate::PdfReader, &path).unwrap();
    let properties = doc.properties().unwrap();
    assert_eq!(properties.attachments, ["data.csv"]);
    assert_eq!(
        properties.files,
        [mireader_reader_sdk::Attachment { name: "data.csv".into(), size: Some(8), modified: Some("2026-10-29".into()) }]
    );
    let out = dir.path().join("out.csv");
    doc.save_attachment(0, &out).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"a,b\n1,2\n");
    assert!(doc.save_attachment(1, &out).is_err(), "no second one");
}

/// Round 20 (#176): a layer is listed with its state, and switching it
/// off takes its content off the page (and on again brings it back); the
/// file is not changed.
#[test]
fn a_switched_off_layer_leaves_the_page() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("layered.pdf");
    std::fs::write(&path, fixtures::build_layer_and_attachment()).unwrap();
    let mut doc = mireader_reader_sdk::Reader::open(&mut crate::PdfReader, &path).unwrap();
    let states = doc.properties().unwrap().layer_states;
    assert_eq!(states, [mireader_reader_sdk::Layer { name: "Notes".into(), on: true }]);
    let blue = |doc: &mut Box<dyn Document>| {
        let page = doc.render_page(0, 1.0).unwrap();
        let px = fixtures::pixel(&page.png, 100, 50);
        px[2] > 200 && px[0] < 80
    };
    assert!(blue(&mut doc), "on: the rectangle shows");
    doc.set_layers(&[false]).unwrap();
    assert!(!blue(&mut doc), "off: it doesn't");
    assert!(!doc.properties().unwrap().layer_states[0].on, "and says so");
    doc.set_layers(&[true]).unwrap();
    assert!(blue(&mut doc), "on again");
    assert!(doc.set_layers(&[true, false]).is_err(), "one state per layer");
    assert!(!doc.info().edit.is_some_and(|e| e.state.dirty), "the document isn't changed");
}

/// Round 20 (#178): a signature is verified from the file's own bytes —
/// intact as signed, its signer named but the identity unverifiable
/// offline (no trust list, no network: never a silent "valid"); a byte
/// changed inside the signed range breaks it; bytes added after signing
/// are reported, not hidden.
#[test]
fn a_signature_is_verified_from_the_files_own_bytes() {
    use mireader_reader_sdk::Integrity;
    use pdfcraft_sign::{der::Time, keys::PrivateKey, pdf::SignOptions, pkcs12::DigitalId, x509::{Certificate, Name}};
    // Test only: the reader verifies, it never signs.
    let key = PrivateKey::generate_p256().unwrap();
    let certificate = Certificate::self_signed(&Name::build("Pat Signer", "", "", "", "US"), &key, Time::from_pdf("D:20260101000000Z").unwrap(), 5, &[1]).unwrap();
    let id = DigitalId { key, certificate, chain: Vec::new(), friendly_name: None };
    let doc = pdfcraft_cos::Document::open(std::sync::Arc::new(fixtures::build())).unwrap();
    let options = SignOptions { date: "D:20261029120000Z".into(), ..Default::default() };
    let signed = pdfcraft_sign::pdf::sign(&doc, &id, &options).unwrap();

    let found = crate::properties::signatures(&signed);
    assert_eq!(found.len(), 1, "{found:?}");
    let one = &found[0];
    assert_eq!(one.integrity, Integrity::Intact, "{one:?}");
    assert_eq!(one.signer.as_deref(), Some("Pat Signer"));
    assert_eq!(one.date.as_deref(), Some("2026-10-29"));
    assert!(!one.identity_trusted, "no trust list: the identity is unverifiable offline");
    assert_eq!(one.bytes_after, 0, "the signature covers the whole file");

    // A byte changed inside what was signed: the page's blue becomes cyan.
    let at = signed.windows(8).position(|w| w == b"0 0 1 rg").expect("the page's colour");
    let mut tampered = signed.clone();
    tampered[at + 2] = b'1';
    assert_eq!(crate::properties::signatures(&tampered)[0].integrity, Integrity::Broken, "tampered");

    // Bytes added after signing (free space, or a later save) are reported.
    let mut padded = signed.clone();
    padded.extend_from_slice(b"\n%% after the signature\n");
    let after = &crate::properties::signatures(&padded)[0];
    assert_eq!(after.bytes_after, 24, "{after:?}");
}

/// Round 19 (the corpus sweep, resetform_parent): a push button without an
/// appearance is drawn with its caption and colours, as every form's Reset
/// or Submit button left to the viewer; a button with its own look keeps it.
#[test]
fn a_push_button_without_an_appearance_shows_its_caption() {
    let mut doc = PdfDocument::new(fixtures::build_push_button_without_appearance()).unwrap();
    let text = doc.page_text(0).unwrap().text;
    assert!(text.contains("Reset"), "the caption is on the page: {text:?}");
    let page = doc.render_page(0, 2.0).unwrap();
    // PDF y grows upward: a point (x, y) is pixel (2x, 2(100 - y)).
    let at = |x: u32, y: u32| fixtures::pixel(&page.png, 2 * x, 2 * (100 - y));
    let grey = at(45, 35);
    assert!(grey[..3].iter().all(|c| (200..=240).contains(c)), "its background: {grey:?}");
    let (pixels, width) = fixtures::pixels(&page.png);
    let dark = (2 * 60..2 * 140).flat_map(|x| (2 * 40..2 * 60).map(move |y| (x, y))).filter(|&(x, y)| pixels[(y * width + x) as usize][..3].iter().all(|c| *c < 100)).count();
    assert!(dark > 40, "its caption is drawn: {dark} dark pixels");
    assert!(at(180, 50)[..3].iter().all(|c| *c < 60), "the other button keeps its own black box: {:?}", at(180, 50));
}

/// Round 19 (the corpus sweep, text_field_own_canvas_calc p3): under
/// /NeedAppearances a field with no value keeps the appearance it has —
/// redrawing it from its empty value wiped a calculated field's result.
#[test]
fn a_field_without_a_value_keeps_its_own_appearance() {
    let mut doc = PdfDocument::new(fixtures::build_need_appearances_own_look()).unwrap();
    let page = doc.render_page(0, 2.0).unwrap();
    let inside = fixtures::pixel(&page.png, 200, 100);
    assert!(inside[..3].iter().all(|c| *c < 60), "its own black box is still drawn: {inside:?}");
}

/// The system's Arial, embedded by the tests above (no font is committed).
/// Missing only off Windows: there the tests skip, and say so.
fn system_arial() -> Option<Vec<u8>> {
    let windows = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| r"C:\Windows".into());
    let font = std::fs::read(windows.join("Fonts").join("arial.ttf")).ok();
    if font.is_none() {
        eprintln!("skipped: no system Arial to embed");
    }
    font
}

/// Round 19 (the corpus sweep, text-layer group 2): a Type 3 font without
/// ToUnicode gave no text, though its Encoding names every glyph. hayro
/// read only the ToUnicode (vendored patch 1 adds the names).
#[test]
fn a_type3_fonts_glyph_names_are_its_text() {
    let mut doc = PdfDocument::new(fixtures::build_type3_named()).unwrap();
    assert_eq!(doc.page_text(0).unwrap().text.trim(), "Hi");
}

/// Round 19 (the corpus sweep, french_diacritics, issue918, issue9655): a
/// Type 3 glyph named only by its own code (/a72, /C69) is that code in the
/// standard encoding, as xpdf and pdf.js read it (vendored patch 1).
#[test]
fn a_type3_glyph_named_by_its_code_is_that_code() {
    let mut doc = PdfDocument::new(fixtures::build_type3_numbered()).unwrap();
    assert_eq!(doc.page_text(0).unwrap().text.trim(), "Hi");
}

/// Round 19 (the corpus sweep, issue5896, issue3188): a Type 3 font gives
/// no glyph widths to the scan, which assumed half an em, so glyphs set
/// wider read as separate letters ("P a r a l l e l"). The advance is now
/// measured to the next glyph. Both directions: a tight run stays one word,
/// a word gap still gives a space.
#[test]
fn a_type3_runs_letters_join_and_its_gaps_still_split() {
    let mut doc = PdfDocument::new(fixtures::build_type3_wide()).unwrap();
    let text = doc.page_text(0).unwrap().text;
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    assert_eq!(lines, ["Hi", "H i"], "{text:?}");
}

/// Round 19 (the corpus sweep, text-layer group 4): one malformed token in
/// a ToUnicode CMap's header made hayro drop the whole CMap, and the font
/// gave no text (vendored patch 2 reads the mappings anyway).
#[test]
fn a_cmap_with_a_broken_header_still_maps() {
    let mut doc = PdfDocument::new(fixtures::build_broken_cmap_header()).unwrap();
    assert_eq!(doc.page_text(0).unwrap().text.trim(), "Hi");
}

/// A deadline no test reaches.
fn far() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(3600)
}

/// Round 19 (the corpus sweep, bug1978317): a page hayro takes too long to
/// interpret answers within the budget, "too complex", instead of never;
/// its text then answers at once, and the other pages still draw. The
/// fixture's first page costs seconds in a debug build and a quarter of one
/// in release (its links' object stream is read quadratically); the budget
/// here is 50 ms. Without the budget the render comes back late and drawn,
/// and this fails on its error.
#[test]
fn a_page_over_its_time_budget_answers_too_complex() {
    use std::time::{Duration, Instant};
    let mut doc = PdfDocument::new(fixtures::build_slow_annotations(3000)).unwrap();
    doc.budget = Duration::from_millis(50);
    let started = Instant::now();
    let error = doc.render_page(0, 1.0).map(|_| started.elapsed()).unwrap_err();
    let took = started.elapsed();
    assert_eq!(error.code, ErrorCode::TooComplex, "{error:?}");
    assert_eq!(error.message, "This page is too complex to draw.");
    assert!(took < Duration::from_millis(1000), "answered at the budget: {took:?}");
    let started = Instant::now();
    assert_eq!(doc.page_text(0).unwrap_err().code, ErrorCode::TooComplex);
    assert!(started.elapsed() < Duration::from_millis(100), "the text answers at once: {:?}", started.elapsed());
    doc.budget = super::PAGE_BUDGET;
    let other = doc.render_page(1, 1.0).unwrap();
    assert_eq!(fixtures::pixel(&other.png, 100, 50), [0, 0, 255, 255], "the next page still draws");
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
    assert_eq!(doc.scan(0, far()).unwrap().ground, Ground::Plain);
    assert!(matches!(doc.scan(1, far()).unwrap().ground, Ground::Declared([255, 0, 0, 255])));
    assert_eq!(doc.scan(2, far()).unwrap().ground, Ground::Image);
    assert_eq!(doc.scan(3, far()).unwrap().ground, Ground::Plain);
    // Round 13 (C3): every rendered page tells the host its ground, on
    // either paper.
    use mireader_reader_sdk::{Document as _, PageGround};
    let grounds: Vec<_> = (0..4).map(|ix| doc.render_page_on(ix, 0.5, Paper::Document).unwrap().ground).collect();
    assert_eq!(
        grounds,
        [Some(PageGround::Plain), Some(PageGround::Declared), Some(PageGround::Image), Some(PageGround::Plain)]
    );
    assert_eq!(doc.render_page_on(2, 0.5, DARK).unwrap().ground, Some(PageGround::Image));
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

// Item 19: editing in place.
mod editing {
    use std::path::{Path, PathBuf};

    use mireader_reader_sdk::{
        AnnotationKind, Document, EditKind, EditOp, ErrorCode, GuardKind, MarkupKind, Reader, Target,
    };

    use super::fixtures;
    use crate::PdfReader;

    fn file(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pdf");
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    fn open(path: &Path) -> Box<dyn Document> {
        PdfReader.open(path).unwrap()
    }

    /// The first line of page 1 as one quad (the text layer's space).
    fn first_line(doc: &mut Box<dyn Document>) -> [f32; 8] {
        let text = doc.page_text(0).unwrap();
        let line_end = text.text.find('\n').unwrap_or(text.text.len());
        let count = text.text[..line_end].chars().count();
        let boxes: Vec<[f32; 4]> = text.boxes[..count].iter().copied().filter(|b| b[2] > b[0]).collect();
        let x0 = boxes.iter().map(|b| b[0]).fold(f32::MAX, f32::min);
        let y0 = boxes.iter().map(|b| b[1]).fold(f32::MAX, f32::min);
        let x1 = boxes.iter().map(|b| b[2]).fold(f32::MIN, f32::max);
        let y1 = boxes.iter().map(|b| b[3]).fold(f32::MIN, f32::max);
        [x0, y0, x1, y0, x0, y1, x1, y1]
    }

    /// Run tool `action` on `path` with `params`; the files it wrote.
    fn run_tool(action: &str, path: &Path, params: serde_json::Value) -> Result<(Vec<PathBuf>, Vec<String>), mireader_reader_sdk::ReaderError> {
        let out = path.parent().unwrap().join(format!("out-{action}"));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let mut progress = |_: mireader_reader_sdk::Progress| {};
        let never = || false;
        let params = params.as_object().cloned().unwrap_or_default();
        let mut job = mireader_reader_sdk::Job::new(action, vec![path.to_path_buf()], params, Some(out), &mut progress, &never);
        let output = crate::security::run(&mut job)?;
        Ok((output.files.into_iter().map(PathBuf::from).collect(), output.notes))
    }

    /// Round 20 (#198): protect with both passwords and a permissions
    /// matrix; the copy needs its open password, keeps to its matrix, and
    /// the permissions password lifts it; Remove security needs that one.
    #[test]
    fn a_password_protects_and_the_right_one_removes_it() {
        let (_dir, path) = file(&fixtures::build_rich());
        let (files, notes) = run_tool(
            crate::security::PROTECT,
            &path,
            serde_json::json!({ "open_password": "open", "permissions_password": "owner", "allow_print": true, "allow_copy": false,
                                "allow_comments": true, "allow_forms": true, "allow_changes": false, "allow_pages": false, "encryption": "AES 256-bit" }),
        )
        .unwrap();
        assert!(notes.iter().any(|n| n.contains("AES, 256")), "{notes:?}");
        let protected = &files[0];
        assert_eq!(PdfReader.open(protected).err().expect("needs its password").code, ErrorCode::Encrypted);
        let mut opened = PdfReader.open_with_password(protected, Some("open")).unwrap();
        let p = opened.properties().unwrap().permissions.expect("encrypted");
        assert!(p.print && !p.copy && p.annotate && !p.modify && !p.assemble && !p.owner, "{p:?}");
        let owned = PdfReader.open_with_password(protected, Some("owner")).unwrap().info().permissions.unwrap();
        assert!(owned.owner, "the permissions password lifts it");
        // The open password alone doesn't remove restrictions; the owner's does.
        let refused = run_tool(crate::security::UNPROTECT, protected, serde_json::json!({ "password": "open" })).err().unwrap();
        assert!(refused.message.contains("permissions password"), "{refused:?}");
        let (files, _) = run_tool(crate::security::UNPROTECT, protected, serde_json::json!({ "password": "owner" })).unwrap();
        let mut plain = open(&files[0]);
        assert!(plain.properties().unwrap().permissions.is_none(), "no security left");
        assert_eq!(std::fs::read(&path).unwrap(), fixtures::build_rich(), "the original untouched");
    }

    /// Round 20 (#198): words and a pattern redacted: the text is gone from
    /// the copy (the other reader can't read it back), the rest stays, and
    /// a file with nothing to redact says so.
    #[test]
    fn redaction_removes_the_words_and_patterns_for_good() {
        let (_dir, path) = file(&fixtures::build_rich());
        let mut before = open(&path);
        let text = before.page_text(0).unwrap().text;
        let word = text.split_whitespace().find(|w| w.chars().all(char::is_alphabetic) && w.len() > 4).expect("a word").to_string();
        let keep = text.split_whitespace().find(|w| w.chars().all(char::is_alphabetic) && w.len() > 4 && **w != *word).map(str::to_string);
        let (files, notes) = run_tool(crate::security::REDACT, &path, serde_json::json!({ "words": word.to_uppercase(), "fill": "Black" })).unwrap();
        assert!(notes.iter().any(|n| n.contains("checked")), "{notes:?}");
        let mut after = open(&files[0]);
        let redacted = after.page_text(0).unwrap().text;
        assert!(!redacted.to_lowercase().contains(&word.to_lowercase()), "{word:?} gone: {redacted:?}");
        if let Some(keep) = keep.filter(|k| !k.to_lowercase().contains(&word.to_lowercase())) {
            assert!(redacted.contains(&keep), "{keep:?} stays");
        }
        let raw = std::fs::read(&files[0]).unwrap();
        assert_eq!(raw.windows(5).filter(|w| w == b"%%EOF").count(), 1, "written whole: no earlier revision keeps it");
        let nothing = run_tool(crate::security::REDACT, &path, serde_json::json!({ "words": "zzqqxx-not-there" })).err().unwrap();
        assert!(nothing.message.contains("Nothing to redact"), "{nothing:?}");
    }

    /// Round 20 (#198): text marked for redaction in the reader (saved as a
    /// Redact mark) is what Redact… removes; the mark itself goes too, and
    /// the encryption line stays empty for a plain file.
    #[test]
    fn marks_made_in_the_reader_are_applied() {
        let (_dir, path) = file(&fixtures::build_rich());
        let mut doc = open(&path);
        let quad = first_line(&mut doc);
        let first = doc.page_text(0).unwrap().text.lines().next().unwrap().trim().to_string();
        doc.edit(EditOp::AddMarkup { page: 0, kind: MarkupKind::Redact, quads: vec![quad], color: None, contents: None }).unwrap();
        doc.save().unwrap();
        assert!(doc.properties().unwrap().encryption.is_none());
        let (files, _) = run_tool(crate::security::REDACT, &path, serde_json::json!({})).unwrap();
        let mut after = open(&files[0]);
        let text = after.page_text(0).unwrap().text;
        assert!(!text.contains(&first), "the marked line is gone: {first:?} in {text:?}");
        assert!(after.annotations(0).unwrap().is_empty(), "and the mark with it");
    }

    /// Round 20 (#198): whole pages redacted leave no text on them.
    #[test]
    fn a_whole_page_is_redacted() {
        let (_dir, path) = file(&fixtures::build_rich());
        let (files, _) = run_tool(crate::security::REDACT, &path, serde_json::json!({ "pages": "1" })).unwrap();
        let mut after = open(&files[0]);
        assert!(after.page_text(0).unwrap().text.trim().is_empty(), "page 1 has no text left");
    }

    /// Round 20 (#198): hidden information goes, and what went is said.
    #[test]
    fn hidden_information_is_removed_and_listed() {
        let (_dir, path) = file(&fixtures::build_rich());
        let mut doc = open(&path);
        doc.edit(EditOp::AddNote { page: 0, at: [20.0, 20.0], contents: "private remark".into(), color: None }).unwrap();
        doc.save().unwrap();
        let (files, notes) = run_tool(crate::security::REMOVE_HIDDEN, &path, serde_json::json!({ "comments": true, "metadata": true })).unwrap();
        assert!(!notes.is_empty(), "what went is said");
        assert_eq!(open(&files[0]).annotations(0).unwrap().len(), 0, "the comment is gone");
        let none = run_tool(crate::security::REMOVE_HIDDEN, &path, serde_json::json!({})).err().unwrap();
        assert!(none.message.contains("Choose"), "{none:?}");
    }

    /// Round 20 (#197): a clean copy drops the revisions an incremental
    /// save piled up, is checked by both readers, and leaves the original.
    #[test]
    fn a_clean_copy_is_whole_checked_and_smaller() {
        let (dir, path) = file(&fixtures::build_rich());
        let mut doc = open(&path);
        for n in 0..4 {
            doc.edit(EditOp::AddNote { page: 0, at: [20.0 + 10.0 * n as f32, 20.0], contents: format!("note {n}"), color: None }).unwrap();
            doc.save().unwrap();
            doc.undo().unwrap();
            doc.save().unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut progress = |_: mireader_reader_sdk::Progress| {};
        let never = || false;
        let mut job = mireader_reader_sdk::Job::new(crate::rewrite::ACTION, vec![path.clone()], Default::default(), Some(out.clone()), &mut progress, &never);
        let output = crate::rewrite::run(&mut job).expect("the copy passes its check");
        assert_eq!(output.files.len(), 1);
        assert!(output.notes.iter().any(|n| n.contains("checked")), "{:?}", output.notes);
        let clean = std::fs::read(&output.files[0]).unwrap();
        assert!(clean.len() < before.len(), "the piled-up revisions are gone: {} < {}", clean.len(), before.len());
        assert_eq!(clean.windows(5).filter(|w| w == b"%%EOF").count(), 1, "one revision");
        assert_eq!(std::fs::read(&path).unwrap(), before, "the original untouched");
        let mut copy = PdfReader.open(std::path::Path::new(&output.files[0])).unwrap();
        assert_eq!(copy.annotations(0).unwrap().len(), 0, "the undone notes stay undone");
    }

    /// Round 20 (#197): a damaged file says what was repaired — here its
    /// cross-reference table and trailer are gone altogether.
    #[test]
    fn a_damaged_file_opens_and_says_what_was_repaired() {
        let original = fixtures::build_rich();
        let cut = original.windows(4).rposition(|w| w == b"xref").expect("the xref table");
        let damaged = original[..cut].to_vec();
        let (_dir, path) = file(&damaged);
        let mut doc = open(&path);
        assert!(!doc.info().pages.is_empty(), "the pages are there");
        let properties = doc.properties().unwrap();
        assert!(!properties.repairs.is_empty(), "and the repair is said: {properties:?}");
        assert!(doc.info().edit.expect("readable").rebuilt, "a rebuilt file is never saved over");
    }

    /// Round 20 (#197): cross-reference streams and object streams (what a
    /// full rewrite writes) read like a classic table.
    #[test]
    fn xref_and_object_streams_read() {
        let cos = pdfcraft_cos::Document::open(std::sync::Arc::new(fixtures::build_rich())).unwrap();
        let streamed = pdfcraft_cos::write_full(&cos, &pdfcraft_cos::SaveOptions::default()).unwrap();
        assert!(streamed.windows(5).any(|w| w == b"/XRef"), "an xref stream was written");
        assert!(streamed.windows(7).any(|w| w == b"/ObjStm"), "and an object stream");
        let (_dir, path) = file(&streamed);
        let mut doc = open(&path);
        let (_odir, opath) = file(&fixtures::build_rich());
        let mut plain = open(&opath);
        assert_eq!(doc.info().pages.len(), plain.info().pages.len());
        assert_eq!(doc.page_text(0).unwrap().text, plain.page_text(0).unwrap().text, "the same text");
        assert!(doc.properties().unwrap().repairs.is_empty(), "nothing to repair");
    }

    /// Round 20 (#197): each save is a revision; the list shows them, and an
    /// earlier one is written out as it was — the current file untouched.
    #[test]
    fn earlier_revisions_are_listed_and_written_out_unchanged() {
        let original = fixtures::build_rich();
        let (_dir, path) = file(&original);
        let mut doc = open(&path);
        assert!(doc.properties().unwrap().revisions.is_empty(), "one revision: no list");
        doc.edit(EditOp::AddNote { page: 0, at: [20.0, 20.0], contents: "one".into(), color: None }).unwrap();
        doc.save().unwrap();
        doc.edit(EditOp::AddNote { page: 0, at: [40.0, 40.0], contents: "two".into(), color: None }).unwrap();
        doc.save().unwrap();
        let revisions = doc.properties().unwrap().revisions;
        assert_eq!(revisions.len(), 3, "{revisions:?}");
        assert!(revisions.windows(2).all(|w| w[0].end < w[1].end));
        let now = std::fs::read(&path).unwrap();
        let first = path.with_file_name("first.pdf");
        doc.save_revision(0, &first).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), original, "the first revision is the original file");
        let second = path.with_file_name("second.pdf");
        doc.save_revision(1, &second).unwrap();
        assert_eq!(open(&second).annotations(0).unwrap().len(), 1, "the second has one note");
        assert_eq!(std::fs::read(&path).unwrap(), now, "the file itself untouched");
        assert!(doc.save_revision(9, &first).is_err());
    }

    /// Round 20 (#197): each permission on its own. Comments allowed, but
    /// not changes or taking the pages apart: the comment tools stay, the
    /// Info fields and bookmarks go, the page tools refuse.
    #[test]
    fn each_permission_is_enforced_on_its_own() {
        let mut cos = pdfcraft_cos::Document::open(std::sync::Arc::new(fixtures::build_rich())).unwrap();
        // Bits from 1: modify is 4, assemble 11.
        cos.set_encryption(&pdfcraft_cos::NewEncryption {
            algorithm: pdfcraft_cos::Algorithm::Aes256,
            user_password: "",
            owner_password: "owner",
            permissions: -1 & !(1 << 3) & !(1 << 10),
            encrypt_metadata: true,
            seed: [5; 32],
        })
        .unwrap();
        let locked = pdfcraft_cos::write_full(&cos, &pdfcraft_cos::SaveOptions::default()).unwrap();
        let (_dir, path) = file(&locked);
        let mut doc = open(&path);
        let kinds = doc.info().edit.expect("comments allowed").kinds;
        assert!(kinds.contains(&EditKind::Markup) && kinds.contains(&EditKind::Note), "comments: {kinds:?}");
        assert!(!kinds.contains(&EditKind::Info) && !kinds.contains(&EditKind::Bookmark), "no changes, no bookmarks: {kinds:?}");
        let permissions = doc.properties().unwrap().permissions.expect("encrypted");
        assert!(permissions.annotate && permissions.print && !permissions.modify && !permissions.assemble && !permissions.owner);
        let refused = crate::organize::open(&path).err().expect("the page tools refuse");
        assert!(refused.message.contains("security settings"), "{refused:?}");
    }

    /// Round 19 (the corpus sweep): a file whose structure had to be rebuilt
    /// is never saved over — the writer would rewrite it whole and the
    /// original bytes would be gone. The reader says so, refuses Save, takes
    /// Save As to a new file (the original untouched), and the copy saves.
    #[test]
    fn a_rebuilt_file_is_never_saved_over() {
        let mut original = fixtures::build_rich();
        // Point startxref at nothing: pdfcraft has to rebuild the xref.
        let at = original.windows(9).rposition(|w| w == b"startxref").expect("startxref");
        let tail = original[at..].to_vec();
        let digits = tail.iter().position(u8::is_ascii_digit).expect("offset");
        let end = digits + tail[digits..].iter().position(|b| !b.is_ascii_digit()).unwrap_or(tail.len() - digits);
        original.splice(at + digits..at + end, b"999999".iter().copied());
        let (_dir, path) = file(&original);
        let mut doc = open(&path);
        let edit = doc.info().edit.expect("editable");
        assert!(edit.rebuilt, "the reader says the file was rebuilt");
        doc.edit(EditOp::AddNote { page: 0, at: [20.0, 20.0], contents: "kept apart".into(), color: None }).unwrap();
        assert!(doc.save().is_err(), "no save over a rebuilt original");
        assert_eq!(std::fs::read(&path).unwrap(), original, "the original is untouched");

        let copy = path.with_file_name("copy (edited).pdf");
        doc.save_as(&copy).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original, "still untouched after Save As");
        assert!(!doc.info().edit.expect("editable").rebuilt, "the copy is ours to save");
        doc.edit(EditOp::AddNote { page: 0, at: [40.0, 40.0], contents: "again".into(), color: None }).unwrap();
        doc.save().expect("the copy saves");
        assert_eq!(open(&copy).annotations(0).unwrap().len(), 2);
    }

    /// Round 20 (#177): a form is listed field by field in tab order, with
    /// its widgets in display space; a text field fills (its appearance
    /// drawn again, so the page shows it), survives a save and reopen, and
    /// Clear form takes it back to its default; the reader may fill forms.
    #[test]
    fn a_form_is_filled_saved_and_cleared() {
        use mireader_reader_sdk::{EditKind, FieldKind, FieldValue};
        let (_dir, path) = file(&fixtures::build_without_appearances());
        let mut doc = open(&path);
        assert!(doc.info().edit.expect("editable").kinds.contains(&EditKind::Form), "it has a form to fill");
        let fields = doc.form_fields().unwrap();
        let names: Vec<(&str, FieldKind)> = fields.iter().map(|f| (f.name.as_str(), f.kind)).collect();
        assert_eq!(names, [("name", FieldKind::Text), ("agree", FieldKind::Check)], "in tab order: {fields:?}");
        let name = &fields[0];
        assert_eq!(name.value, ["Hello"]);
        let r = name.widgets[0].rect;
        assert!((r[0] - 20.0).abs() < 0.5 && (r[1] - 10.0).abs() < 0.5 && (r[2] - 180.0).abs() < 0.5 && (r[3] - 40.0).abs() < 0.5, "display space: {r:?}");
        doc.edit(EditOp::SetField { name: "name".into(), value: FieldValue::Text("Worldwide".into()) }).unwrap();
        assert!(doc.page_text(0).unwrap().text.contains("Worldwide"), "the page shows the value");
        doc.save().unwrap();
        let mut doc = open(&path);
        assert_eq!(doc.form_fields().unwrap()[0].value, ["Worldwide"], "kept");
        doc.edit(EditOp::ClearForm).unwrap();
        assert!(doc.form_fields().unwrap()[0].value.iter().all(|v| v.is_empty()), "back to its default (none)");
        assert!(doc.edit(EditOp::SetField { name: "nope".into(), value: FieldValue::Text("x".into()) }).is_err());
    }

    /// Round 20 (#174): a highlight takes new words (QuadPoints and Rect
    /// rewritten, still a highlight), a note moves by an offset, and undo
    /// and redo take each back as one step; a note can't take quads.
    #[test]
    fn a_comment_is_reshaped_and_moved_in_single_steps() {
        let (_dir, path) = file(&fixtures::build());
        let mut doc = open(&path);
        let quad = |x0: f32, y0: f32, x1: f32, y1: f32| [x0, y0, x1, y0, x0, y1, x1, y1];
        let (_, mark) = doc
            .edit(EditOp::AddMarkup { page: 0, kind: mireader_reader_sdk::MarkupKind::Highlight, quads: vec![quad(20.0, 20.0, 60.0, 30.0)], color: None, contents: None })
            .unwrap();
        let mark = mark.unwrap();
        let rect_of = |doc: &mut Box<dyn Document>, id: &str| doc.annotations(0).unwrap().into_iter().find(|a| a.id == id).map(|a| a.rect).unwrap();
        doc.edit(EditOp::SetQuads { id: mark.clone(), quads: vec![quad(20.0, 20.0, 140.0, 30.0), quad(20.0, 34.0, 80.0, 44.0)] }).unwrap();
        let reshaped = doc.annotations(0).unwrap().into_iter().find(|a| a.id == mark).unwrap();
        assert_eq!(reshaped.quads.len(), 2, "a quad per line");
        assert!((reshaped.rect[2] - 140.0).abs() < 0.5 && (reshaped.rect[3] - 44.0).abs() < 0.5, "{:?}", reshaped.rect);
        assert_eq!(reshaped.kind, mireader_reader_sdk::AnnotationKind::Highlight);
        doc.undo().unwrap();
        assert!((rect_of(&mut doc, &mark)[2] - 60.0).abs() < 0.5, "one undo step");
        doc.redo().unwrap();
        assert!((rect_of(&mut doc, &mark)[2] - 140.0).abs() < 0.5, "and redo");

        let (_, note) = doc.edit(EditOp::AddNote { page: 0, at: [100.0, 50.0], contents: "here".into(), color: None }).unwrap();
        let note = note.unwrap();
        let before = rect_of(&mut doc, &note);
        doc.edit(EditOp::Move { id: note.clone(), dx: 12.0, dy: 5.0 }).unwrap();
        let after = rect_of(&mut doc, &note);
        assert!((after[0] - before[0] - 12.0).abs() < 0.5 && (after[1] - before[1] - 5.0).abs() < 0.5, "{before:?} -> {after:?}");
        assert!(doc.edit(EditOp::SetQuads { id: note, quads: vec![quad(1.0, 1.0, 2.0, 2.0)] }).is_err(), "a note has no words");
    }

    /// Round 20 (#173): a note's thread — a reply lists under its parent
    /// (`in_reply_to`), with its own date; it can be edited and deleted;
    /// the parent can't be deleted alone while it has replies, only with
    /// them; the thread survives a save and reopen; another reader's reply
    /// is listed read only.
    #[test]
    fn a_note_keeps_a_thread_of_replies() {
        let (_dir, path) = file(&fixtures::build());
        let mut doc = open(&path);
        let (_, parent) = doc.edit(EditOp::AddNote { page: 0, at: [20.0, 20.0], contents: "first".into(), color: None }).unwrap();
        let parent = parent.expect("the note's id");
        let (_, reply) = doc.edit(EditOp::AddReply { parent: parent.clone(), contents: "a reply".into() }).unwrap();
        let reply = reply.expect("the reply's id");
        let listed = doc.annotations(0).unwrap();
        let r = listed.iter().find(|a| a.id == reply).expect("the reply is listed");
        assert_eq!(r.in_reply_to.as_deref(), Some(parent.as_str()));
        assert_eq!(r.contents.as_deref(), Some("a reply"));
        assert!(r.editable && r.modified.is_some(), "ours, dated: {r:?}");
        doc.edit(EditOp::SetContents { id: reply.clone(), contents: "edited".into() }).unwrap();
        assert!(doc.edit(EditOp::Delete { id: parent.clone() }).is_err(), "not alone while it has replies");
        doc.save().unwrap();

        let mut doc = open(&path);
        let listed = doc.annotations(0).unwrap();
        let r = listed.iter().find(|a| a.id == reply).expect("after reopening");
        assert_eq!((r.in_reply_to.as_deref(), r.contents.as_deref()), (Some(parent.as_str()), Some("edited")));
        doc.edit(EditOp::DeleteThread { id: parent.clone() }).unwrap();
        assert!(doc.annotations(0).unwrap().iter().all(|a| a.id != parent && a.id != reply), "the thread goes whole");
    }

    /// Round 20 (#173): a reply another reader wrote is shown, read only.
    #[test]
    fn another_readers_reply_is_read_only() {
        let (_dir, path) = file(&fixtures::build());
        let mut doc = open(&path);
        let (_, parent) = doc.edit(EditOp::AddNote { page: 0, at: [20.0, 20.0], contents: "first".into(), color: None }).unwrap();
        let (_, reply) = doc.edit(EditOp::AddReply { parent: parent.unwrap(), contents: "theirs".into() }).unwrap();
        doc.save().unwrap();
        // Another program's name on the reply.
        let bytes = std::fs::read(&path).unwrap();
        let ours = b"(MimiReader)";
        let at = bytes.windows(ours.len()).rposition(|w| w == ours).expect("the reply's author");
        let mut theirs = bytes.clone();
        theirs.splice(at..at + ours.len(), b"(OtherApp00)".iter().copied());
        std::fs::write(&path, &theirs).unwrap();
        let mut doc = open(&path);
        let listed = doc.annotations(0).unwrap();
        let r = listed.iter().find(|a| Some(&a.id) == reply.as_ref()).expect("listed, not dropped");
        assert_eq!(r.author.as_deref(), Some("OtherApp00"));
        assert!(!r.editable, "read only");
    }

    /// Round 19 (the corpus sweep, P2): the appearances drawn for display
    /// are never saved. The editor works on the file's own bytes, so a save
    /// after an edit appends only the edit, and the field still has no /AP.
    #[test]
    fn drawn_appearances_stay_out_of_the_saved_file() {
        let original = fixtures::build_without_appearances();
        let (_dir, path) = file(&original);
        let mut doc = open(&path);
        assert!(doc.page_text(0).unwrap().text.contains("Hello"), "drawn for display");
        doc.edit(EditOp::AddNote { page: 0, at: [100.0, 80.0], contents: "noted".into(), color: None }).unwrap();
        doc.save().unwrap();
        let saved = std::fs::read(&path).unwrap();
        assert!(saved.starts_with(&original), "appended: the original bytes are intact");
        let reread = pdfcraft_cos::Document::open(std::sync::Arc::new(saved)).unwrap();
        for field in pdfcraft_forms::fields(&reread) {
            for widget in &field.widgets {
                let widget = reread.get(widget.obj);
                assert!(widget.as_dict().is_some_and(|d| !d.contains(b"AP")), "{} saved without an appearance", field.name);
            }
        }
        assert!(open(&path).page_text(0).unwrap().text.contains("Hello"), "and drawn again on reopening");
    }

    /// Round 19 (T1 item 6): the four Info fields, set in one step — one
    /// undo puts them all back — and saved as an incremental update: the
    /// original file is still the saved file's first bytes, and reopening
    /// reads the new values; an empty field is removed.
    #[test]
    fn info_fields_are_one_undo_step_and_an_appended_update() {
        let original = fixtures::build_rich();
        let (_dir, path) = file(&original);
        let mut doc = open(&path);
        let before = doc.properties().unwrap();
        let set = |title: &str, author: &str| EditOp::SetInfo {
            title: title.into(),
            author: author.into(),
            subject: "Field notes".into(),
            keywords: "".into(),
        };
        let (state, _) = doc.edit(set("新しい題名 New title", "Ada")).unwrap();
        assert!(state.dirty && state.can_undo);
        let now = doc.properties().unwrap();
        assert_eq!(now.title.as_deref(), Some("新しい題名 New title"));
        assert_eq!(now.author.as_deref(), Some("Ada"));
        assert_eq!(now.subject.as_deref(), Some("Field notes"));
        assert_eq!(now.keywords, None, "an empty field is removed");

        // One step: one undo restores every field.
        doc.undo().unwrap();
        let undone = doc.properties().unwrap();
        assert_eq!((undone.title, undone.author, undone.subject, undone.keywords), (before.title, before.author, before.subject, before.keywords));
        doc.redo().unwrap();

        doc.save().unwrap();
        let saved = std::fs::read(&path).unwrap();
        assert!(saved.len() > original.len() && saved.starts_with(&original), "appended: the original bytes are intact");
        let reopened = open(&path).properties().unwrap();
        assert_eq!(reopened.title.as_deref(), Some("新しい題名 New title"));
        assert_eq!(reopened.author.as_deref(), Some("Ada"));
        assert_eq!(reopened.keywords, None);
    }

    /// A highlight over the text layer's first line, a note, a recolour, a
    /// bookmark — undone, redone, saved as an incremental update and read
    /// back from the file.
    #[test]
    fn comments_and_bookmarks_round_trip_through_a_save() {
        let original = fixtures::build_rich();
        let (_dir, path) = file(&original);
        let mut doc = open(&path);
        let edit = doc.info().edit.expect("editable");
        assert_eq!(edit.kinds, vec![EditKind::Markup, EditKind::Note, EditKind::Bookmark, EditKind::Info]);
        assert!(edit.guard.is_none());

        let quad = first_line(&mut doc);
        let (state, highlight) = doc
            .edit(EditOp::AddMarkup { page: 0, kind: MarkupKind::Highlight, quads: vec![quad], color: None, contents: None })
            .unwrap();
        let highlight = highlight.expect("an id");
        assert!(state.dirty && state.can_undo && !state.can_redo);
        let (_, note) = doc
            .edit(EditOp::AddNote { page: 0, at: [250.0, 20.0], contents: "Check this".into(), color: None })
            .unwrap();
        doc.edit(EditOp::SetColor { id: highlight.clone(), color: [0.4, 0.9, 0.4] }).unwrap();
        doc.edit(EditOp::AddBookmark { parent: vec![], index: 0, title: "First look".into(), page: 1 }).unwrap();

        let items = doc.annotations(0).unwrap();
        assert_eq!(items.len(), 2, "{items:?}");
        let mark = items.iter().find(|a| a.id == highlight).expect("the highlight");
        assert_eq!(mark.kind, AnnotationKind::Highlight);
        assert!(mark.editable);
        let back = mark.quads[0];
        for (a, b) in back.iter().zip(quad.iter()) {
            assert!((a - b).abs() < 0.5, "quad round trip {back:?} vs {quad:?}");
        }
        assert_eq!(mark.color, Some([0.4, 0.9, 0.4]));
        let pinned = items.iter().find(|a| Some(&a.id) == note.as_ref()).expect("the note");
        assert_eq!(pinned.kind, AnnotationKind::Note);
        assert_eq!(pinned.contents.as_deref(), Some("Check this"));
        assert!((pinned.rect[0] - 250.0).abs() < 1.0 && (pinned.rect[1] - 20.0).abs() < 1.0, "{:?}", pinned.rect);
        assert_eq!(doc.info().outline[0].title, "First look");
        assert_eq!(doc.info().outline[0].target, Target::Page(1));

        // Undo the bookmark and the recolour; redo the recolour.
        doc.undo().unwrap();
        assert!(doc.info().outline.iter().all(|e| e.title != "First look"));
        doc.undo().unwrap();
        assert_ne!(doc.annotations(0).unwrap().iter().find(|a| a.id == highlight).unwrap().color, Some([0.4, 0.9, 0.4]));
        let state = doc.redo().unwrap();
        assert!(state.can_redo, "the bookmark can still be redone");
        assert_eq!(doc.annotations(0).unwrap().iter().find(|a| a.id == highlight).unwrap().color, Some([0.4, 0.9, 0.4]));

        // The page renders (hayro draws the comments' appearances).
        doc.render_page(0, 1.0).unwrap();

        let state = doc.save().unwrap();
        assert!(!state.dirty);
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("doc.pdf")], "no temp file is left behind");
        let saved = std::fs::read(&path).unwrap();
        assert!(saved.starts_with(&original), "an incremental update keeps the original bytes");
        let mut reread = open(&path);
        let items = reread.annotations(0).unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|a| a.id == highlight), "ids survive a save: {items:?}");
        assert!(!reread.info().edit.unwrap().state.dirty);
    }

    /// Deleting a comment, and saving again after a save (the second update
    /// appends to the first).
    #[test]
    fn delete_and_a_second_save_append() {
        let (_dir, path) = file(&fixtures::build_rich());
        let mut doc = open(&path);
        let quad = first_line(&mut doc);
        let (_, id) = doc
            .edit(EditOp::AddMarkup { page: 0, kind: MarkupKind::StrikeOut, quads: vec![quad], color: None, contents: None })
            .unwrap();
        doc.save().unwrap();
        let first = std::fs::read(&path).unwrap();
        doc.edit(EditOp::Delete { id: id.unwrap() }).unwrap();
        assert!(doc.annotations(0).unwrap().is_empty());
        doc.save().unwrap();
        let second = std::fs::read(&path).unwrap();
        assert!(second.starts_with(&first), "the second save appends to the first");
        assert!(open(&path).annotations(0).unwrap().is_empty());
    }

    /// The file changed on disk since it was opened: the save refuses and
    /// writes nothing; a touch that leaves the bytes alone is no conflict.
    #[test]
    fn a_save_refuses_a_file_changed_on_disk() {
        let original = fixtures::build_rich();
        let (_dir, path) = file(&original);
        let mut doc = open(&path);
        let quad = first_line(&mut doc);
        doc.edit(EditOp::AddMarkup { page: 0, kind: MarkupKind::Underline, quads: vec![quad], color: None, contents: None })
            .unwrap();
        let mut other = fixtures::build();
        std::fs::write(&path, &other).unwrap();
        let error = doc.save().unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict, "{error:?}");
        assert_eq!(std::fs::read(&path).unwrap(), other, "nothing written");
        // Put the original bytes back (a touch): saving goes through.
        std::fs::write(&path, &original).unwrap();
        doc.save().unwrap();
        other = std::fs::read(&path).unwrap();
        assert!(other.starts_with(&original));
    }

    /// An encrypted PDF needs its password; edits then carry an
    /// "encrypted" note and save.
    #[test]
    fn an_encrypted_pdf_opens_with_its_password_and_edits() {
        let mut cos = pdfcraft_cos::Document::open(std::sync::Arc::new(fixtures::build_rich())).unwrap();
        cos.set_encryption(&pdfcraft_cos::NewEncryption {
            algorithm: pdfcraft_cos::Algorithm::Aes256,
            user_password: "open sesame",
            owner_password: "owner",
            permissions: -1,
            encrypt_metadata: true,
            seed: [3; 32],
        })
        .unwrap();
        let encrypted = pdfcraft_cos::write_full(&cos, &pdfcraft_cos::SaveOptions::default()).unwrap();
        let (_dir, path) = file(&encrypted);
        let error = PdfReader.open(&path).err().expect("needs a password");
        assert_eq!(error.code, ErrorCode::Encrypted);
        let mut doc = PdfReader.open_with_password(&path, Some("open sesame")).unwrap();
        let guard = doc.info().edit.unwrap().guard.expect("a note");
        assert_eq!(guard.kind, GuardKind::Encrypted);
        let quad = first_line(&mut doc);
        doc.edit(EditOp::AddMarkup { page: 0, kind: MarkupKind::Highlight, quads: vec![quad], color: None, contents: None })
            .unwrap();
        doc.save().unwrap();
        let mut reread = PdfReader.open_with_password(&path, Some("open sesame")).unwrap();
        assert_eq!(reread.annotations(0).unwrap().len(), 1);
    }

    /// A signed PDF edits with a warning; a certified one (no changes)
    /// refuses.
    #[test]
    fn signed_pdfs_warn_and_certified_ones_refuse() {
        let signed = with_signature(None);
        let (_dir, path) = file(&signed);
        let doc = open(&path);
        let edit = doc.info().edit.unwrap();
        assert_eq!(edit.guard.as_ref().map(|g| g.kind), Some(GuardKind::Signed));
        assert!(!edit.kinds.is_empty(), "comments are allowed");

        let certified = with_signature(Some(1));
        let (_dir2, path) = file(&certified);
        let mut doc = open(&path);
        let edit = doc.info().edit.unwrap();
        assert_eq!(edit.guard.as_ref().map(|g| g.kind), Some(GuardKind::Permissions));
        assert!(edit.kinds.is_empty());
        let refused = doc
            .edit(EditOp::AddNote { page: 0, at: [10.0, 10.0], contents: "x".into(), color: None })
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::Unsupported);
    }

    /// The rich fixture plus a filled signature field (and, with `certify`,
    /// a DocMDP certification with that `P`).
    fn with_signature(certify: Option<i64>) -> Vec<u8> {
        use pdfcraft_cos::{Dict, Object, write_incremental};
        let mut cos = pdfcraft_cos::Document::open(std::sync::Arc::new(fixtures::build_rich())).unwrap();
        let mut sig = Dict::new();
        sig.set("Type", Object::name("Sig"));
        sig.set("Filter", Object::name("Adobe.PPKLite"));
        if let Some(p) = certify {
            let mut params = Dict::new();
            params.set("P", Object::Int(p));
            let mut reference = Dict::new();
            reference.set("TransformMethod", Object::name("DocMDP"));
            reference.set("TransformParams", Object::Dict(params));
            sig.set("Reference", Object::Array(vec![Object::Dict(reference)]));
        }
        let sig_ref = cos.add(Object::Dict(sig));
        let mut field = Dict::new();
        field.set("FT", Object::name("Sig"));
        field.set("T", Object::String(pdfcraft_cos::PdfString::text("Signature1")));
        field.set("V", Object::Ref(sig_ref));
        let field_ref = cos.add(Object::Dict(field));
        let mut form = Dict::new();
        form.set("Fields", Object::Array(vec![Object::Ref(field_ref)]));
        form.set("SigFlags", Object::Int(3));
        let root = cos.root().unwrap();
        cos.update_dict(root, |catalog| {
            catalog.set("AcroForm", Object::Dict(form));
            if certify.is_some() {
                let mut perms = Dict::new();
                perms.set("DocMDP", Object::Ref(sig_ref));
                catalog.set("Perms", Object::Dict(perms));
            }
        })
        .unwrap();
        write_incremental(&cos, &pdfcraft_cos::SaveOptions::default()).unwrap()
    }
}

// MIREADER (round 14, #5): links on the page.

#[test]
fn links_come_with_the_page_text() {
    use mireader_reader_sdk::LinkTarget;
    let mut doc = PdfDocument::new(fixtures::build_linked()).unwrap();
    let links = doc.page_text(0).unwrap().links;
    let got: Vec<([f32; 4], LinkTarget)> = links.into_iter().map(|l| (l.rect, l.target)).collect();
    assert_eq!(
        got,
        vec![
            // Top-left page points: y = 200 - PDF y.
            ([20.0, 30.0, 120.0, 60.0], LinkTarget::Page { page: 2, top: Some(80.0) }),
            ([20.0, 80.0, 120.0, 100.0], LinkTarget::Page { page: 0, top: Some(50.0) }),
            ([20.0, 120.0, 120.0, 140.0], LinkTarget::Uri { uri: "https://example.org/x".into() }),
        ],
        "a /Launch action and a note are not links"
    );
    // A turned page: the link sits inside its turned 200×300 area, turned too.
    let turned = doc.page_text(1).unwrap().links;
    assert_eq!(turned.len(), 1);
    let [x0, y0, x1, y1] = turned[0].rect;
    assert!(x0 >= 0.0 && y0 >= 0.0 && x1 <= 200.0 && y1 <= 300.0, "{:?}", turned[0].rect);
    assert_eq!(((x1 - x0).round(), (y1 - y0).round()), (20.0, 50.0), "{:?}", turned[0].rect);
    assert_eq!(turned[0].target, LinkTarget::Page { page: 0, top: None });
    assert!(doc.page_text(2).unwrap().links.is_empty());
}

#[test]
fn outline_targets_are_unchanged_by_the_shared_resolver() {
    let doc = PdfDocument::new(fixtures::build_rich()).unwrap();
    let pages: Vec<Target> = doc.info().outline.iter().map(|e| e.target.clone()).collect();
    assert_eq!(pages, vec![Target::Page(0), Target::Page(1)]);
}

/// Round 14 (#5): a real file's links, listed — `MIREADER_LINKS_PDF=<path>
/// cargo test -- --ignored --nocapture real_links`.
#[test]
#[ignore = "needs a real PDF"]
fn real_links() {
    let Ok(path) = std::env::var("MIREADER_LINKS_PDF") else { return };
    let mut doc = PdfDocument::new(std::fs::read(path).unwrap()).unwrap();
    let mut total = 0;
    for page in 0..doc.info().pages.len() as u32 {
        let links = doc.page_text(page).unwrap().links;
        total += links.len();
        if !links.is_empty() && page < 6 {
            println!("page {page}: {} links, first {:?}", links.len(), &links[..links.len().min(3)]);
        }
    }
    println!("total links: {total}");
    assert!(total > 0);
}

/// Round 18 (#99): where a printed page's time goes inside the reader —
/// the raster (hayro) and its PNG encode, at the printer's 300 dpi:
/// `MIREADER_TEST_PDF=<a.pdf> cargo test --release print_raster_parts -- --ignored --nocapture`.
#[test]
#[ignore = "needs a sample PDF"]
fn print_raster_parts() {
    use hayro::{PixmapSettings, RenderCache, RenderSettings, hayro_interpret::InterpreterSettings, vello_cpu::color::palette::css::WHITE};
    let path = std::env::var("MIREADER_TEST_PDF").expect("MIREADER_TEST_PDF");
    let doc = PdfDocument::new(std::fs::read(&path).unwrap()).unwrap();
    let pages = doc.pdf.pages();
    let count = pages.len().min(40);
    let (mut raster, mut encode, mut bytes) = (std::time::Duration::ZERO, std::time::Duration::ZERO, 0usize);
    for page in pages.iter().take(count) {
        let (width, height) = page.render_dimensions();
        let scale = clamp_scale(width, height, 300.0 / 72.0);
        let started = std::time::Instant::now();
        let pixmap = hayro::render(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE },
        );
        raster += started.elapsed();
        let started = std::time::Instant::now();
        let png = pixmap.into_png().unwrap();
        encode += started.elapsed();
        bytes += png.len();
    }
    let per = |d: std::time::Duration| d.as_secs_f64() * 1000.0 / count as f64;
    eprintln!("{count} pages at 300 dpi: raster {:.1} ms/page, PNG encode {:.1} ms/page, {:.0} KB/page", per(raster), per(encode), bytes as f64 / 1e3 / count as f64);
}

/// Round 19 (T1 item 5): what a page thumbnail costs the reader — the
/// whole `render_page` (raster and PNG) at the app's three thumbnail widths
/// on a 1.5× screen, the first 40 pages, and the decoded size the app holds:
/// `MIREADER_TEST_PDF=<a.pdf> cargo test --release thumbnail_cost -- --ignored --nocapture`.
#[test]
#[ignore = "needs a sample PDF"]
fn thumbnail_cost() {
    let path = std::env::var("MIREADER_TEST_PDF").expect("MIREADER_TEST_PDF");
    let mut doc = PdfDocument::new(std::fs::read(&path).unwrap()).unwrap();
    let sizes = doc.info().pages.clone();
    let count = sizes.len().min(40);
    for width in [96.0f32, 140.0, 200.0] {
        let (mut total, mut pixels, mut worst) = (std::time::Duration::ZERO, 0usize, std::time::Duration::ZERO);
        for page in 0..count {
            let scale = width * 1.5 / sizes[page].width;
            let started = std::time::Instant::now();
            let rendered = doc.render_page(page as u32, scale).unwrap();
            let took = started.elapsed();
            total += took;
            worst = worst.max(took);
            pixels += (rendered.width * rendered.height) as usize;
        }
        eprintln!(
            "{width} px thumbnails, {count} pages: {:.1} ms each (worst {:.1}), {:.0} KB decoded each",
            total.as_secs_f64() * 1000.0 / count as f64,
            worst.as_secs_f64() * 1000.0,
            pixels as f64 * 4.0 / 1e3 / count as f64
        );
    }
}
