//! MiReader PDF reader: a paged document rasterised with `hayro` (pure
//! Rust, MIT/Apache — no MuPDF/poppler). The host asks for page *n* at a
//! scale; the reader answers with a PNG.
//!
//! Password-protected PDFs that open with the empty user password (the
//! common "permissions only" case) read normally; others are refused with
//! `Encrypted`.

mod outline;
mod recolor;
mod scan;

use std::{collections::VecDeque, path::Path};

use hayro::{
    PixmapSettings, RenderCache, RenderSettings,
    hayro_interpret::{InterpreterCache, InterpreterSettings},
    hayro_syntax::{LoadPdfError, Pdf},
    kurbo::Rect,
    vello_cpu::color::palette::css::{TRANSPARENT, WHITE},
};
use mireader_reader_sdk::{
    Document, DocumentInfo, ErrorCode, PROTOCOL_VERSION, PageGround, PageSize, PageText, Paper, Reader, ReaderError,
    ReaderInfo, RenderedPage,
};
use scan::{Ground, Scan};

pub const ID: &str = "mireader.pdf";

/// Ceiling on one rendered page, in pixels (≈ 48 MB of RGBA) — a huge
/// page at a high zoom is rendered at the largest scale that fits.
const MAX_PIXELS: f32 = 12_000_000.0;
/// The pixmap's side is a `u16`.
const MAX_SIDE: f32 = 16_384.0;

#[derive(Default)]
pub struct PdfReader;

impl Reader for PdfReader {
    fn info(&self) -> ReaderInfo {
        ReaderInfo {
            protocol: PROTOCOL_VERSION,
            id: ID.into(),
            name: "PDF".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            extensions: vec!["pdf".into()],
        }
    }

    fn open(&mut self, path: &Path) -> Result<Box<dyn Document>, ReaderError> {
        Ok(Box::new(PdfDocument::new(std::fs::read(path)?)?))
    }
}

/// Page scans kept for reuse (text layer, ground, image rects). A scan is
/// cheap next to a render, but a long document's worth of glyphs is not.
const SCANS_KEPT: usize = 24;

pub struct PdfDocument {
    pdf: Pdf,
    info: DocumentInfo,
    /// Most recently used last.
    scans: VecDeque<(u32, Scan)>,
}

/// A PDF text string: UTF-16BE with a BOM, UTF-8 with a BOM, or
/// PDFDocEncoding (Latin-1 is close enough for titles).
fn decode_text(bytes: &[u8]) -> String {
    if let Some(utf16) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = utf16.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else if let Some(utf8) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(utf8).into_owned()
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

impl PdfDocument {
    pub fn new(bytes: Vec<u8>) -> Result<Self, ReaderError> {
        let pdf = Pdf::new(bytes).map_err(|error| match error {
            LoadPdfError::Decryption(_) => {
                ReaderError::new(ErrorCode::Encrypted, "this PDF is password-protected")
            }
            LoadPdfError::Invalid => ReaderError::invalid("not a readable PDF"),
        })?;
        let pages: Vec<PageSize> = pdf
            .pages()
            .iter()
            .map(|page| {
                let (width, height) = page.render_dimensions();
                PageSize { width, height }
            })
            .collect();
        if pages.is_empty() {
            return Err(ReaderError::invalid("the PDF has no pages"));
        }
        let mut info = DocumentInfo::paged(pages);
        let metadata = pdf.metadata();
        let text = |field: &Option<Vec<u8>>| {
            field
                .as_deref()
                .map(decode_text)
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        };
        info.title = text(&metadata.title);
        info.author = text(&metadata.author);
        info.outline = outline::read(&pdf);
        Ok(Self {
            pdf,
            info,
            scans: VecDeque::new(),
        })
    }

    /// The scan of page `ix`, interpreted once and kept for a while.
    fn scan(&mut self, ix: u32) -> Result<&Scan, ReaderError> {
        if let Some(at) = self.scans.iter().position(|(page, _)| *page == ix) {
            let entry = self.scans.remove(at).expect("position is in range");
            self.scans.push_back(entry);
        } else {
            let pages = self.pdf.pages();
            let page = pages
                .get(ix as usize)
                .ok_or_else(|| ReaderError::bad_request(format!("no page {ix}")))?;
            let scan = Scan::page(page, &InterpreterCache::new());
            if self.scans.len() >= SCANS_KEPT {
                self.scans.pop_front();
            }
            self.scans.push_back((ix, scan));
        }
        Ok(&self.scans.back().expect("just pushed").1)
    }
}

/// The scale actually used for a page of `width`×`height` points.
pub fn clamp_scale(width: f32, height: f32, scale: f32) -> f32 {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let by_area = (MAX_PIXELS / (width * height).max(1.0)).sqrt();
    let by_side = MAX_SIDE / width.max(height).max(1.0);
    scale.min(by_area).min(by_side)
}

impl Document for PdfDocument {
    fn info(&self) -> DocumentInfo {
        self.info.clone()
    }

    fn render_page(&mut self, page: u32, scale: f32) -> Result<RenderedPage, ReaderError> {
        self.render_page_on(page, scale, Paper::Document)
    }

    fn render_page_on(&mut self, ix: u32, scale: f32, paper: Paper) -> Result<RenderedPage, ReaderError> {
        // Round 13 (C3): every page is classified so the host can say why a
        // page keeps its own colours. The scan is cached and the text layer
        // needs it next anyway.
        let scan = self.scan(ix)?;
        let ground = match scan.ground {
            Ground::Plain => PageGround::Plain,
            Ground::Declared(_) => PageGround::Declared,
            Ground::Image => PageGround::Image,
        };
        let themed = match paper {
            Paper::Theme { background, ink } => {
                (scan.ground == Ground::Plain).then(|| (background, ink, scan.images.clone()))
            }
            Paper::Document => None,
        };
        let pages = self.pdf.pages();
        let page = pages
            .get(ix as usize)
            .ok_or_else(|| ReaderError::bad_request(format!("no page {ix}")))?;
        let (width, height) = page.render_dimensions();
        let scale = clamp_scale(width, height, scale);
        let mut pixmap = hayro::render(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                // A themed plain page is drawn on nothing: the host's ground
                // shows through.
                bg_color: if themed.is_some() { TRANSPARENT } else { WHITE },
            },
        );
        let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        if let Some((background, ink, images)) = themed {
            let s = scale as f64;
            let images: Vec<Rect> = images
                .iter()
                .map(|r| Rect::new(r.x0 * s, r.y0 * s, r.x1 * s, r.y1 * s))
                .collect();
            recolor::remap(pixmap.data_as_u8_slice_mut(), width, height, background, ink, &images);
        }
        let png = pixmap
            .into_png()
            .map_err(|e| ReaderError::new(ErrorCode::Internal, format!("PNG encoding failed: {e}")))?;
        Ok(RenderedPage { width, height, png, ground: Some(ground) })
    }

    fn page_text(&mut self, page: u32) -> Result<PageText, ReaderError> {
        Ok(self.scan(page)?.text())
    }

    /// The page's plain text (search).
    fn text(&mut self, index: u32) -> Result<String, ReaderError> {
        Ok(self.page_text(index)?.text)
    }
}
#[cfg(test)]
mod tests;
