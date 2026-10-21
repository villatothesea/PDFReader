//! MiReader PDF reader: a paged document rasterised with `hayro` (pure
//! Rust, MIT/Apache — no MuPDF/poppler). The host asks for page *n* at a
//! scale; the reader answers with a PNG.
//!
//! Password-protected PDFs that open with the empty user password (the
//! common "permissions only" case) read normally; others are refused with
//! `Encrypted`.

use std::path::Path;

use hayro::{
    PixmapSettings, RenderCache, RenderSettings, hayro_interpret::InterpreterSettings,
    hayro_syntax::{LoadPdfError, Pdf},
    vello_cpu::color::palette::css::WHITE,
};
use mireader_reader_sdk::{
    Document, DocumentInfo, ErrorCode, PROTOCOL_VERSION, PageSize, Reader, ReaderError, ReaderInfo,
    RenderedPage,
};

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

pub struct PdfDocument {
    pdf: Pdf,
    info: DocumentInfo,
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
        Ok(Self { pdf, info })
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
        let pages = self.pdf.pages();
        let page = pages
            .get(page as usize)
            .ok_or_else(|| ReaderError::bad_request(format!("no page {page}")))?;
        let (width, height) = page.render_dimensions();
        let scale = clamp_scale(width, height, scale);
        let pixmap = hayro::render(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );
        let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        let png = pixmap
            .into_png()
            .map_err(|e| ReaderError::new(ErrorCode::Internal, format!("PNG encoding failed: {e}")))?;
        Ok(RenderedPage { width, height, png })
    }
}

#[cfg(test)]
mod tests;
