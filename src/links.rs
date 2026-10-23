//! Round 14 (#5): a page's links — `/Link` annotations in its `/Annots`,
//! with a `/Dest`, a `/GoTo` action (a printed contents entry, a
//! cross-reference) or a `/URI` action (a web address) — as areas in the
//! page points the text layer uses: top-left origin, after the rotation.

use hayro::hayro_interpret::{RectExt as _, TransformExt as _};
use hayro::hayro_syntax::{
    Pdf,
    object::{Array, Dict, Name, Rect, String as PdfString},
};
use hayro::kurbo::Point;
use mireader_reader_sdk::{LinkTarget, PageLink};

use crate::outline::{Dests, page_ids};

/// Most links read from one page — a malformed file can't stall a page.
const MAX_LINKS: usize = 2_000;
/// Longest address kept.
const MAX_URI: usize = 4_096;

pub fn read(pdf: &Pdf, page: u32) -> Vec<PageLink> {
    let pages = pdf.pages();
    let Some(this) = pages.get(page as usize) else {
        return Vec::new();
    };
    let Some(annots) = this.raw().get::<Array<'_>>(b"Annots") else {
        return Vec::new();
    };
    let xref = pdf.xref();
    let Some(catalog) = xref.get::<Dict<'_>>(xref.root_id()) else {
        return Vec::new();
    };
    let ids = page_ids(pdf);
    let dests = Dests { catalog: &catalog, pages: &ids };
    let transform = this.initial_transform(true).to_kurbo();
    let mut out = Vec::new();
    for annot in annots.iter::<Dict<'_>>().take(MAX_LINKS) {
        if !annot.get::<Name<'_>>(b"Subtype").is_some_and(|s| s.as_ref() == b"Link") {
            continue;
        }
        let Some(rect) = annot.get::<Rect>(b"Rect") else { continue };
        let target = if let Some((to, top)) = dests.of(&annot) {
            // The destination's top, in the target page's own points.
            let top = top.zip(pages.get(to as usize)).map(|(top, target)| {
                let point = target.initial_transform(true).to_kurbo() * Point::new(0.0, top);
                point.y.max(0.0) as f32
            });
            LinkTarget::Page { page: to, top }
        } else if let Some(uri) = uri(&annot) {
            LinkTarget::Uri { uri }
        } else {
            continue;
        };
        let area = transform.transform_rect_bbox(rect.to_kurbo());
        if area.width() <= 0.0 || area.height() <= 0.0 {
            continue;
        }
        out.push(PageLink {
            rect: [area.x0 as f32, area.y0 as f32, area.x1 as f32, area.y1 as f32],
            target,
        });
    }
    out
}

/// A `/URI` action's address (ASCII by the spec; kept as read).
fn uri(annot: &Dict<'_>) -> Option<String> {
    let action = annot.get::<Dict<'_>>(b"A")?;
    if !action.get::<Name<'_>>(b"S").is_some_and(|s| s.as_ref() == b"URI") {
        return None;
    }
    let bytes = action.get::<PdfString<'_>>(b"URI")?;
    let uri = String::from_utf8_lossy(bytes.as_bytes()).trim().to_string();
    (!uri.is_empty() && uri.len() <= MAX_URI).then_some(uri)
}
