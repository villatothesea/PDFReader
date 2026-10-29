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
// Round 19 (the corpus sweep): text from an embedded font's own cmap.
mod font_text;
// Round 19 (the corpus sweep, P2): the appearances a file leaves to the viewer.
mod display;
// MimiReader round 19 (O12, T1 item 6): Document Properties.
mod properties;
// Round 17 (O6): page organising — the reader's own tools.
pub mod organize;
// MimiReader round 18: Convert to Markdown, from the retired PDFInspector.
pub mod convert;
// MimiReader round 19 (O12): pages as images, the text layer as plain text.
pub mod export;

use std::{
    collections::{HashSet, VecDeque},
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use hayro::{
    PixmapSettings, RenderCache, RenderSettings,
    hayro_interpret::InterpreterCache,
    hayro_syntax::{LoadPdfError, Pdf, page::Page},
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
/// Round 19 (the corpus sweep, bug1978317): the longest one call may spend
/// interpreting a page, to draw it or to read its text. The corpus's next
/// slowest whole file takes 7 s; the host gives up on a call at 60 s and
/// restarts the reader, which no answer at all would cost.
const PAGE_BUDGET: Duration = Duration::from_secs(20);
/// What a page over [`PAGE_BUDGET`] answers, for its drawing and its text.
const TOO_COMPLEX: &str = "This page is too complex to draw.";

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
    /// Shared with a page's worker thread (see [`PdfDocument::within_budget`]).
    pdf: Arc<Pdf>,
    info: DocumentInfo,
    /// Most recently used last.
    scans: VecDeque<(u32, Scan)>,
    /// Item 19: the edit session, when the file can be written back.
    editor: Option<edit::Editor>,
    password: Option<String>,
    /// Round 19 (bug1978317): pages that ran over the budget. They answer
    /// at once from then on; their work may still be running, and asking
    /// again would only start it twice.
    too_complex: HashSet<u32>,
    budget: Duration,
    /// Round 20 (#176): the layers the reader switched (index-aligned with
    /// `properties::layers`); `None`: the file's own default.
    layers_on: Option<Vec<bool>>,
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
    /// Round 20 (#176): each switched layer's group and whether it shows,
    /// for the renderer (empty: the file's own default).
    fn layer_overrides(&self) -> std::sync::Arc<Vec<(hayro::hayro_syntax::object::ObjectIdentifier, bool)>> {
        let (Some(on), Some(editor)) = (&self.layers_on, &self.editor) else {
            return Default::default();
        };
        std::sync::Arc::new(
            properties::layers(editor.document())
                .into_iter()
                .zip(on)
                .map(|((reference, ..), on)| (hayro::hayro_syntax::object::ObjectIdentifier::new(reference.num as i32, i32::from(reference.generation)), *on))
                .collect(),
        )
    }

    pub fn new(bytes: Vec<u8>) -> Result<Self, ReaderError> {
        Self::with_password(bytes, None)
    }

    pub fn with_password(bytes: Vec<u8>, password: Option<&str>) -> Result<Self, ReaderError> {
        // Round 19 (P2): drawn with the appearances the file leaves to the
        // viewer; the editor keeps the file's own bytes.
        let bytes = match password {
            None => display::with_appearances(&bytes).unwrap_or(bytes),
            Some(_) => bytes,
        };
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
            pdf: Arc::new(pdf),
            info,
            scans: VecDeque::new(),
            editor: None,
            password: None,
            too_complex: HashSet::new(),
            budget: PAGE_BUDGET,
            layers_on: None,
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
        self.too_complex.clear();
        Ok(())
    }

    fn editor(&mut self) -> Result<&mut edit::Editor, ReaderError> {
        self.editor
            .as_mut()
            .ok_or_else(|| ReaderError::unsupported("this PDF can't be edited"))
    }

    /// Round 19 (the corpus sweep, bug1978317): `work` on page `ix`, on a
    /// thread of its own, waited for until `deadline`. hayro can't be
    /// stopped part-way, so a page over the budget is left to finish on its
    /// own and answers [`ErrorCode::TooComplex`], now and from then on.
    fn within_budget<T: Send + 'static>(
        &mut self,
        ix: u32,
        deadline: Instant,
        work: impl FnOnce(&Page<'_>) -> T + Send + 'static,
    ) -> Result<T, ReaderError> {
        let too_complex = || ReaderError::new(ErrorCode::TooComplex, TOO_COMPLEX);
        if self.too_complex.contains(&ix) {
            return Err(too_complex());
        }
        if self.pdf.pages().get(ix as usize).is_none() {
            return Err(ReaderError::bad_request(format!("no page {ix}")));
        }
        let pdf = self.pdf.clone();
        let (sender, answer) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name(format!("pdf-page-{ix}"))
            .spawn(move || {
                let _ = sender.send(work(&pdf.pages()[ix as usize]));
            })
            .map_err(|e| ReaderError::new(ErrorCode::Internal, format!("no thread for the page: {e}")))?;
        match answer.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(done) => Ok(done),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.too_complex.insert(ix);
                Err(too_complex())
            }
            // The work panicked: re-raised here, where the server's guard
            // turns it into this request's error, as before.
            Err(mpsc::RecvTimeoutError::Disconnected) => match worker.join() {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(()) => unreachable!("the worker sends before it ends"),
            },
        }
    }

    /// The scan of page `ix`, interpreted once and kept for a while.
    fn scan(&mut self, ix: u32, deadline: Instant) -> Result<&Scan, ReaderError> {
        if let Some(at) = self.scans.iter().position(|(page, _)| *page == ix) {
            let entry = self.scans.remove(at).expect("position is in range");
            self.scans.push_back(entry);
        } else {
            let scan = self.within_budget(ix, deadline, |page| Scan::page(page, &InterpreterCache::new()))?;
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
            Some(editor) => {
                let mut properties = properties::read(editor.document());
                // Round 20 (#178): the signatures, from the file's own bytes.
                properties.signatures = properties::signatures(editor.disk_bytes());
                // Round 20 (#176): what shows now, not the file's default.
                if let Some(on) = &self.layers_on {
                    for (layer, on) in properties.layer_states.iter_mut().zip(on) {
                        layer.on = *on;
                    }
                }
                properties
            }
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

    /// Round 20 (#175): the attachment, decoded, written to `path` (the
    /// host asked before replacing anything there).
    fn save_attachment(&mut self, index: usize, path: &std::path::Path) -> Result<(), ReaderError> {
        let editor = self.editor.as_ref().ok_or_else(|| ReaderError::unsupported("this document's attachments can't be read"))?;
        let bytes = properties::attachment_bytes(editor.document(), index).map_err(ReaderError::invalid)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// Round 20 (#176): the layers that show from now on — a view setting:
    /// the file isn't changed (saving a new default is a later step).
    fn set_layers(&mut self, on: &[bool]) -> Result<(), ReaderError> {
        let editor = self.editor.as_ref().ok_or_else(|| ReaderError::unsupported("this document's layers can't be read"))?;
        let count = properties::layers(editor.document()).len();
        if on.len() != count {
            return Err(ReaderError::bad_request(format!("{count} layers, {} states", on.len())));
        }
        self.layers_on = Some(on.to_vec());
        Ok(())
    }

    fn render_page_on(&mut self, ix: u32, scale: f32, paper: Paper) -> Result<RenderedPage, ReaderError> {
        // Round 13 (C3): every page is classified so the host can say why a
        // page keeps its own colours. The scan is cached and the text layer
        // needs it next anyway.
        let deadline = Instant::now() + self.budget;
        let scan = self.scan(ix, deadline)?;
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
        // A themed plain page is drawn on nothing: the host's ground shows
        // through.
        let bg_color = if themed.is_some() { TRANSPARENT } else { WHITE };
        // Round 20 (#176): the layers the reader switched.
        let overrides = self.layer_overrides();
        let (scale, mut pixmap) = self.within_budget(ix, deadline, move |page| {
            let (width, height) = page.render_dimensions();
            let scale = clamp_scale(width, height, scale);
            let settings = PixmapSettings { x_scale: scale, y_scale: scale, bg_color };
            let mut interpreter = crate::fallback::settings();
            interpreter.ocg_overrides = overrides;
            let pixmap = hayro::render(page, &RenderCache::new(), &interpreter, &RenderSettings::default(), &settings);
            (scale, pixmap)
        })?;
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
        let mut text = self.scan(page, Instant::now() + self.budget)?.text();
        // Round 14 (#5): the page's links ride with its text.
        text.links = links::read(&self.pdf, page);
        Ok(text)
    }

    /// The page's plain text (search).
    fn text(&mut self, index: u32) -> Result<String, ReaderError> {
        Ok(self.scan(index, Instant::now() + self.budget)?.text().text)
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
