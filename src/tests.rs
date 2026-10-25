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
