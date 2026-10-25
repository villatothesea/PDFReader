//! MiReader PDF reader: a paged document rasterised with `hayro` (pure
//! Rust, MIT/Apache — no MuPDF/poppler). The host asks for page *n* at a
//! scale; the reader answers with a PNG.
//!
//! Password-protected PDFs that open with the empty user password (the
//! common "permissions only" case) read normally; others are refused with
//! `Encrypted`.

mod outline;
// Round 14 (#5): links on the page.
mod links;
// Item 19: comments and bookmarks written back in place.
mod edit;
mod recolor;
mod scan;
// Round 19 (the corpus sweep): a system font for non-embedded CJK text.
mod fallback;
// MimiReader round 19 (O12, T1 item 6): Document Properties.
mod properties;
// Round 17 (O6): page organising — the reader's own tools.
pub mod organize;
// MimiReader round 18: Convert to Markdown, from the retired PDFInspector.
pub mod convert;
// MimiReader round 19 (O12): pages as images, the text layer as plain text.
pub mod export;

use std::{collections::VecDeque, path::Path, sync::Arc};

use hayro::{
    PixmapSettings, RenderCache, RenderSettings,
    hayro_interpret::InterpreterCache,
    hayro_syntax::{LoadPdfError, Pdf},
    kurbo::Rect,
    vello_cpu::color::palette::css::{TRANSPARENT, WHITE},
};
use mireader_reader_sdk::{
    Annotation, Document, DocumentInfo, EditOp, EditState, ErrorCode, PROTOCOL_VERSION, PageGround, PageSize, PageText,
    Paper, Reader, ReaderError, ReaderInfo, RenderedPage,
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
        self.open_with_password(path, None)
    }

    fn open_with_password(&mut self, path: &Path, password: Option<&str>) -> Result<Box<dyn Document>, ReaderError> {
        let bytes = std::fs::read(path)?;
        let mut doc = PdfDocument::with_password(bytes.clone(), password)?;
        // Item 19: editable when pdfcraft can write the file back.
        doc.editor = edit::Editor::open(path, Arc::new(bytes), password);
        doc.password = password.map(str::to_owned);
        doc.info.page_labels = page_labels(doc.editor.as_ref(), doc.info.pages.len());
        Ok(Box::new(doc))
    }

    /// Round 17 (O6): the reader also acts — extract, reorder, split and
    /// merge pages (its manifest's `[[actions]]`). MimiReader round 18: and
    /// converts to Markdown.
    fn run_job(&mut self, job: &mut mireader_reader_sdk::Job<'_>) -> Result<mireader_reader_sdk::ToolOutput, ReaderError> {
        if job.action == convert::ACTION {
            return convert::run(job);
        }
        if matches!(job.action, export::TO_TEXT | export::TO_IMAGES) {
            return export::run(job);
        }
        organize::run(job)
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
    /// Item 19: the edit session, when the file can be written back.
    editor: Option<edit::Editor>,
    password: Option<String>,
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
        Self::with_password(bytes, None)
    }

    pub fn with_password(bytes: Vec<u8>, password: Option<&str>) -> Result<Self, ReaderError> {
        let pdf = Pdf::new_with_password(bytes, password.unwrap_or("")).map_err(|error| match error {
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
            editor: None,
            password: None,
        })
    }

    /// Item 19: read the edited document again — its pages, outline and
    /// text — after an edit, an undo or a save.
    fn reload(&mut self) -> Result<(), ReaderError> {
        let Some(editor) = &self.editor else { return Ok(()) };
        let bytes = editor.bytes()?;
        let fresh = Self::with_password(bytes, self.password.as_deref())?;
        self.pdf = fresh.pdf;
        self.info.pages = fresh.info.pages;
        self.info.outline = fresh.info.outline;
        self.info.page_labels = page_labels(self.editor.as_ref(), self.info.pages.len());
        self.scans.clear();
        Ok(())
    }

    fn editor(&mut self) -> Result<&mut edit::Editor, ReaderError> {
        self.editor
            .as_mut()
            .ok_or_else(|| ReaderError::unsupported("this PDF can't be edited"))
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

/// Round 19 (O12, T1 item 3): the document's own page numbers — `i`, `ii`,
/// `1` … — read through the editor's copy of the file (pdfcraft parses it
/// there anyway). Empty when they are just 1 … n, or unreadable: the host
/// then numbers pages itself.
fn page_labels(editor: Option<&edit::Editor>, count: usize) -> Vec<String> {
    let Some(labels) = editor.and_then(edit::Editor::page_labels) else { return Vec::new() };
    let plain = labels.iter().enumerate().all(|(ix, label)| *label == (ix + 1).to_string());
    if plain || labels.len() != count { Vec::new() } else { labels }
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
        let mut info = self.info.clone();
        info.edit = self.editor.as_ref().map(edit::Editor::info);
        info
    }

    /// MimiReader round 19 (O12, T1 item 6): read through pdfcraft (the
    /// editor's copy, which is current after edits); without it, what the
    /// renderer read.
    fn properties(&mut self) -> Result<mireader_reader_sdk::DocumentProperties, ReaderError> {
        Ok(match &self.editor {
            Some(editor) => properties::read(editor.document()),
            None => mireader_reader_sdk::DocumentProperties {
                title: self.info.title.clone(),
                author: self.info.author.clone(),
                ..Default::default()
            },
        })
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
            &crate::fallback::settings(),
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
        let mut text = self.scan(page)?.text();
        // Round 14 (#5): the page's links ride with its text.
        text.links = links::read(&self.pdf, page);
        Ok(text)
    }

    /// The page's plain text (search).
    fn text(&mut self, index: u32) -> Result<String, ReaderError> {
        Ok(self.scan(index)?.text().text)
    }

    fn annotations(&mut self, page: u32) -> Result<Vec<Annotation>, ReaderError> {
        self.editor()?.annotations(page)
    }

    fn edit(&mut self, op: EditOp) -> Result<(EditState, Option<String>), ReaderError> {
        let created = self.editor()?.apply(op)?;
        self.reload()?;
        Ok((self.editor()?.state(), created))
    }

    fn undo(&mut self) -> Result<EditState, ReaderError> {
        if self.editor()?.undo() {
            self.reload()?;
        }
        Ok(self.editor()?.state())
    }

    fn redo(&mut self) -> Result<EditState, ReaderError> {
        if self.editor()?.redo() {
            self.reload()?;
        }
        Ok(self.editor()?.state())
    }

    fn save(&mut self) -> Result<EditState, ReaderError> {
        self.editor()?.save()?;
        Ok(self.editor()?.state())
    }

    fn save_as(&mut self, path: &Path) -> Result<EditState, ReaderError> {
        self.editor()?.save_as(path)?;
        Ok(self.editor()?.state())
    }
}
#[cfg(test)]
mod tests;
