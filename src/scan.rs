//! One interpretation pass over a page with a collecting [`Device`]: the
//! positioned text (for selection, copy and search), where raster images
//! sit, and what kind of ground the page has (for [`Paper::Theme`]).
//!
//! Coordinates are page points with the origin at the top-left, after the
//! page's rotation — `page.initial_transform(true)`, the space the host
//! lays pages out in.
//!
//! [`Paper::Theme`]: mireader_reader_sdk::Paper::Theme

use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, Paint, SoftMask, TransformExt as _,
    font::{Glyph, GlyphRun},
    interpret_page,
};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{BezPath, Point, Rect, Shape};
use mireader_reader_sdk::PageText;

/// Glyph-space box of one em, before the glyph's own advance: PDF fonts are
/// laid out on 1000 units per em, descender to ascender ≈ −200…800.
const EM_BOTTOM: f64 = -200.0;
const EM_TOP: f64 = 800.0;
/// Advance assumed for glyphs whose font reports none.
const FALLBACK_ADVANCE: f64 = 500.0;
/// A first fill this much of the page or larger is the page's ground.
const GROUND_COVERAGE: f64 = 0.9;
/// Raster images covering this much of the page make it an image page.
const IMAGE_PAGE_COVERAGE: f64 = 0.6;

/// What a page is drawn on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ground {
    /// No ground of its own (or a plain white one): paper.
    Plain,
    /// The page paints its own coloured background first.
    Declared([u8; 4]),
    /// The page is mostly raster images (a scan, a photo page).
    Image,
}

/// One drawn glyph.
#[derive(Debug, Clone)]
pub struct ScannedGlyph {
    pub text: String,
    /// Bounding box in page points (top-left origin).
    pub rect: Rect,
    /// Baseline start and direction (unit vector), in page points.
    pub origin: Point,
    pub direction: (f64, f64),
    /// Em height in page points.
    pub size: f64,
}

#[derive(Debug, Clone)]
pub struct Scan {
    pub glyphs: Vec<ScannedGlyph>,
    /// Raster images, page points.
    pub images: Vec<Rect>,
    pub ground: Ground,
}

impl Scan {
    /// Interpret `page` once and collect.
    pub fn page<'a>(page: &Page<'a>, cache: &InterpreterCache<'a>) -> Self {
        let (width, height) = page.render_dimensions();
        let transform = page.initial_transform(true).to_kurbo();
        let mut context = Context::new(
            transform,
            Rect::new(0.0, 0.0, width as f64, height as f64),
            cache,
            page.xref(),
            crate::fallback::settings(),
        );
        let mut collector = Collector::new(Rect::new(0.0, 0.0, width as f64, height as f64));
        interpret_page(page, &mut context, &mut collector);
        collector.finish()
    }

    /// The page's text in reading order, one box per char.
    pub fn text(&self) -> PageText {
        assemble(&self.glyphs)
    }
}

struct Collector {
    page: Rect,
    glyphs: Vec<ScannedGlyph>,
    images: Vec<Rect>,
    /// Decided on the first paint operation.
    ground: Option<Ground>,
}

impl Collector {
    fn new(page: Rect) -> Self {
        Self {
            page,
            glyphs: Vec::new(),
            images: Vec::new(),
            ground: None,
        }
    }

    fn page_area(&self) -> f64 {
        self.page.area().max(1.0)
    }

    /// The first thing painted on the page decides whether it is the ground.
    fn first_paint(&mut self, bounds: Rect, color: Option<[u8; 4]>) {
        if self.ground.is_some() {
            return;
        }
        let covered = bounds.intersect(self.page).area() / self.page_area();
        self.ground = Some(match color {
            Some(rgba) if covered >= GROUND_COVERAGE && rgba[3] > 200 && !near_white(rgba) => {
                Ground::Declared(rgba)
            }
            _ => Ground::Plain,
        });
    }

    fn finish(mut self) -> Scan {
        let image_area: f64 = self
            .images
            .iter()
            .map(|rect| rect.intersect(self.page).area())
            .sum();
        let ground = match self.ground.take() {
            Some(Ground::Declared(color)) => Ground::Declared(color),
            _ if image_area / self.page_area() >= IMAGE_PAGE_COVERAGE => Ground::Image,
            _ => Ground::Plain,
        };
        Scan {
            glyphs: dedupe(self.glyphs),
            images: self.images,
            ground,
        }
    }
}

fn near_white(rgba: [u8; 4]) -> bool {
    rgba[..3].iter().all(|channel| *channel >= 245)
}

fn solid(paint: &Paint<'_>) -> Option<[u8; 4]> {
    match paint {
        Paint::Color(color) => Some(color.to_rgba().to_rgba8()),
        Paint::Pattern(_) => None,
    }
}

impl<'a> Device<'a> for Collector {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode) {
        if matches!(draw_mode, DrawMode::Invisible) {
            return;
        }
        let bounds = (props.transform * path.clone()).bounding_box();
        let fill = matches!(draw_mode, DrawMode::Fill(_) | DrawMode::FillAndStroke(..));
        self.first_paint(bounds, fill.then(|| solid(&props.paint)).flatten());
    }

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, draw_mode: &DrawMode) {
        // Stroked text is the outline of text already collected (fill +
        // stroke draws the run twice); `dedupe` drops the copies.
        let _ = draw_mode;
        for glyph in run.glyphs() {
            let transform = props.transform * glyph.transform();
            let (text, advance) = match &**glyph {
                Glyph::Outline(outline) => (
                    outline
                        .as_unicode()
                        .map(bf_string)
                        .or_else(|| crate::font_text::from_font_cmap(outline)),
                    outline.advance_width().map(f64::from).filter(|w| *w > 0.0),
                ),
                // Round 19 (issue5896): the font's own width (vendored patch
                // 3), not a guessed half em that split words into letters.
                Glyph::Type3(type3) => (
                    type3.as_unicode().map(bf_string),
                    type3.advance_width().map(f64::from).filter(|w| *w > 0.0),
                ),
            };
            let advance = advance.unwrap_or(FALLBACK_ADVANCE);
            let corners = [
                Point::new(0.0, EM_BOTTOM),
                Point::new(advance, EM_BOTTOM),
                Point::new(advance, EM_TOP),
                Point::new(0.0, EM_TOP),
            ]
            .map(|point| transform * point);
            let rect = corners
                .iter()
                .skip(1)
                .fold(Rect::from_points(corners[0], corners[0]), |rect, point| {
                    rect.union_pt(*point)
                });
            if self.ground.is_none() && !matches!(draw_mode, DrawMode::Invisible) {
                self.ground = Some(Ground::Plain);
            }
            let Some(text) = text.filter(|text| !text.is_empty() && !text.chars().all(char::is_control))
            else {
                continue;
            };
            // Round 19 (the corpus sweep, issue1350): the text is what the
            // page shows. A glyph whose centre lies outside the page's box
            // (its crop box, in this space) isn't on the page — pages that
            // draw the same content through different crop boxes each gave
            // the whole of it, so search found matches on pages that don't
            // show them.
            if !self.page.contains(rect.center()) {
                continue;
            }
            let origin = transform * Point::new(0.0, 0.0);
            let along = transform * Point::new(1000.0, 0.0) - origin;
            let up = transform * Point::new(0.0, 1000.0) - origin;
            let length = along.hypot().max(f64::EPSILON);
            self.glyphs.push(ScannedGlyph {
                text,
                rect,
                origin,
                direction: (along.x / length, along.y / length),
                size: up.hypot(),
            });
        }
    }

    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        let bounds = (props.transform
            * Rect::new(0.0, 0.0, image.width() as f64, image.height() as f64).to_path(0.1))
        .bounding_box();
        match image {
            // A stencil is ink (a glyph bitmap, a logo mask) drawn in the
            // current colour: it recolours with the text.
            Image::Stencil(_) => self.first_paint(bounds, None),
            Image::Raster(_) => {
                if self.ground.is_none() {
                    self.ground = Some(Ground::Plain);
                }
                self.images.push(bounds);
            }
        }
    }

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}

fn bf_string(text: hayro::hayro_interpret::hayro_cmap::BfString) -> String {
    use hayro::hayro_interpret::hayro_cmap::BfString;
    match text {
        BfString::Char(c) => c.to_string(),
        BfString::String(s) => s,
    }
}

/// Fake-bold and fill-then-stroke draw the same glyph more than once, a hair
/// apart. Keep the first of each.
fn dedupe(glyphs: Vec<ScannedGlyph>) -> Vec<ScannedGlyph> {
    const WINDOW: usize = 12;
    let mut kept: Vec<ScannedGlyph> = Vec::with_capacity(glyphs.len());
    for glyph in glyphs {
        let duplicate = kept.iter().rev().take(WINDOW).any(|other| {
            other.text == glyph.text && {
                let overlap = other.rect.intersect(glyph.rect).area();
                overlap > 0.7 * glyph.rect.area().max(other.rect.area()).max(f64::EPSILON)
            }
        });
        if !duplicate {
            kept.push(glyph);
        }
    }
    kept
}

/// Glyphs in drawing order → text with the spaces and line breaks PDFs
/// rarely encode, one box per `char`.
fn assemble(glyphs: &[ScannedGlyph]) -> PageText {
    let mut out = PageText::default();
    let mut previous: Option<&ScannedGlyph> = None;
    for glyph in glyphs {
        if let Some(prev) = previous {
            match separator(prev, glyph) {
                Some('\n') if !out.text.ends_with('\n') => push_gap(&mut out, '\n'),
                Some(' ') if !out.text.ends_with([' ', '\n']) && !glyph.text.starts_with(' ') => {
                    push_gap(&mut out, ' ')
                }
                _ => {}
            }
        }
        let chars: Vec<char> = glyph.text.chars().collect();
        // A ligature ("ﬁ" mapped to "fi") shares its box among its chars.
        let step = glyph.rect.width() / chars.len() as f64;
        for (ix, ch) in chars.iter().enumerate() {
            let horizontal = glyph.direction.0.abs() >= glyph.direction.1.abs();
            let rect = if horizontal && chars.len() > 1 {
                let x0 = if glyph.direction.0 >= 0.0 {
                    glyph.rect.x0 + step * ix as f64
                } else {
                    glyph.rect.x1 - step * (ix + 1) as f64
                };
                Rect::new(x0, glyph.rect.y0, x0 + step, glyph.rect.y1)
            } else {
                glyph.rect
            };
            out.text.push(*ch);
            out.boxes.push(rounded(rect));
        }
        previous = Some(glyph);
    }
    out
}

fn push_gap(out: &mut PageText, ch: char) {
    let at = out.boxes.last().map_or([0.0; 4], |b| [b[2], b[1], b[2], b[3]]);
    out.text.push(ch);
    out.boxes.push(at);
}

fn rounded(rect: Rect) -> [f32; 4] {
    let r = |v: f64| ((v * 100.0).round() / 100.0) as f32;
    [r(rect.x0), r(rect.y0), r(rect.x1), r(rect.y1)]
}

/// What separates `next` from `prev`: a line break when it leaves the
/// line, a space when there is a gap on the same line.
fn separator(prev: &ScannedGlyph, next: &ScannedGlyph) -> Option<char> {
    let size = prev.size.max(next.size).max(1.0);
    let (dx, dy) = (next.origin.x - prev.origin.x, next.origin.y - prev.origin.y);
    let (ux, uy) = prev.direction;
    // Distance off the baseline, and progress along it.
    let across = (-uy * dx + ux * dy).abs();
    let along = ux * dx + uy * dy;
    if across > 0.5 * size || along < -0.5 * size {
        return Some('\n');
    }
    // The gap between the end of `prev` and the start of `next`.
    let prev_end = match (ux.abs() >= uy.abs(), ux >= 0.0, uy >= 0.0) {
        (true, true, _) => prev.rect.x1,
        (true, false, _) => -prev.rect.x0,
        (false, _, true) => prev.rect.y1,
        (false, _, false) => -prev.rect.y0,
    };
    let next_start = match (ux.abs() >= uy.abs(), ux >= 0.0, uy >= 0.0) {
        (true, true, _) => next.rect.x0,
        (true, false, _) => -next.rect.x1,
        (false, _, true) => next.rect.y0,
        (false, _, false) => -next.rect.y1,
    };
    (next_start - prev_end > 0.15 * size).then_some(' ')
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro::hayro_syntax::Pdf;

    /// What a page's glyphs gave the text layer, by font mechanism.
    #[derive(Default)]
    struct Probe {
        outline: usize,
        outline_mapped: usize,
        outline_with_data: usize,
        type3: usize,
        type3_mapped: usize,
        paths: usize,
        images: usize,
    }

    impl<'a> Device<'a> for Probe {
        fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {
            self.paths += 1;
        }
        fn push_clip_path(&mut self, _: &ClipPath) {}
        fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
        fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {
            for glyph in run.glyphs() {
                match &**glyph {
                    Glyph::Outline(outline) => {
                        self.outline += 1;
                        self.outline_mapped += usize::from(outline.as_unicode().is_some());
                        self.outline_with_data += usize::from(outline.font_data().is_some());
                    }
                    Glyph::Type3(type3) => {
                        self.type3 += 1;
                        self.type3_mapped += usize::from(type3.as_unicode().is_some());
                    }
                }
            }
        }
        fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {
            self.images += 1;
        }
        fn pop_clip(&mut self) {}
        fn pop_transparency_group(&mut self) {}
    }

    /// Round 19 (the corpus sweep): per listed page (`path<TAB>page`, 1-based),
    /// how many glyphs were drawn and how many gave text — a report for
    /// grouping empty text layers by cause, not a check.
    /// `MIREADER_GLYPH_LIST=… MIREADER_GLYPH_OUT=… cargo test --lib glyph_mapping_report -- --ignored`.
    #[test]
    #[ignore = "a report over the corpus, not a check"]
    fn glyph_mapping_report() {
        let (Ok(list), Ok(out)) = (std::env::var("MIREADER_GLYPH_LIST"), std::env::var("MIREADER_GLYPH_OUT")) else { return };
        let mut lines = Vec::new();
        for line in std::fs::read_to_string(list).unwrap_or_default().lines() {
            let Some((path, page)) = line.trim().split_once('\t') else { continue };
            let Ok(page) = page.parse::<usize>() else { continue };
            let Ok(bytes) = std::fs::read(path) else { continue };
            let Ok(pdf) = Pdf::new(bytes) else {
                lines.push(format!("{path}\t{page}\tunopenable"));
                continue;
            };
            let Some(page_obj) = pdf.pages().get(page - 1) else {
                lines.push(format!("{path}\t{page}\tno such page"));
                continue;
            };
            let (width, height) = page_obj.render_dimensions();
            let cache = InterpreterCache::new();
            let mut context = Context::new(
                page_obj.initial_transform(true).to_kurbo(),
                Rect::new(0.0, 0.0, width as f64, height as f64),
                &cache,
                page_obj.xref(),
                crate::fallback::settings(),
            );
            let mut probe = Probe::default();
            interpret_page(page_obj, &mut context, &mut probe);
            lines.push(format!(
                "{path}\t{page}\toutline={} mapped={} data={} type3={} t3mapped={} paths={} images={}",
                probe.outline,
                probe.outline_mapped,
                probe.outline_with_data,
                probe.type3,
                probe.type3_mapped,
                probe.paths,
                probe.images
            ));
        }
        let _ = std::fs::write(out, lines.join("\n"));
    }
}
