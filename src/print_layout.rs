//! Round 20 (#199): a print layout — the pages imposed on sheets as a
//! print-ready PDF of their own.
//!
//! Several pages per sheet (with borders if wanted), a folded booklet
//! (page order reads right once folded; left or right binding), a poster
//! tiled across sheets (scale, overlap, cut marks); all pages, odd or even
//! ones only, in reverse — the manual double-sided workflow (for a
//! booklet: fronts, then backs); the document with or without its
//! comments, or the form fields alone; grayscale, and printing as images
//! (every page rendered first: for a printer that mangles a page).
//!
//! The layout is written beside the original and opens in the app: that is
//! the preview, and printing it (Ctrl+P) prints exactly what it shows. A
//! duplex printer's two-sided setting is in the system print dialog.

use std::sync::Arc;

use mireader_reader_sdk::{Document as _, ErrorCode, Job, Paper, ReaderError, ToolOutput};
use pdfcraft_cos::{Document as Cos, SaveOptions, write_full};
use pdfcraft_print::{Binding, BookletSubset, Content, Layout, PageOrder, Settings, Subset};

use crate::PdfDocument;

pub const ACTION: &str = "print-layout";

fn invalid(why: impl std::fmt::Display) -> ReaderError {
    ReaderError::invalid(why.to_string())
}

/// A page's PNG in grey (luminance; alpha kept as white paper).
fn to_grey(png: &[u8]) -> Result<Vec<u8>, ReaderError> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().map_err(invalid)?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or_else(|| invalid("a page too large"))?];
    let info = reader.next_frame(&mut buf).map_err(invalid)?;
    let channels = info.color_type.samples();
    let grey: Vec<u8> = buf[..info.buffer_size()]
        .chunks_exact(channels)
        .map(|px| {
            let (r, g, b) = if channels >= 3 { (px[0] as f32, px[1] as f32, px[2] as f32) } else { (px[0] as f32, px[0] as f32, px[0] as f32) };
            let a = if channels == 4 || channels == 2 { px[channels - 1] as f32 / 255.0 } else { 1.0 };
            let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            (y * a + 255.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8
        })
        .collect();
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, info.width, info.height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().and_then(|mut w| w.write_image_data(&grey)).map_err(invalid)?;
    }
    Ok(out)
}

pub fn run(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let source = job.path().to_path_buf();
    let bytes = std::fs::read(&source)?;
    let mut doc = Cos::open(Arc::new(bytes.clone())).map_err(|error| invalid(format!("It can't be read: {error}")))?;
    if doc.permissions().is_some_and(|p| !p.print()) {
        return Err(ReaderError::new(ErrorCode::Unsupported, "This PDF's security settings don't allow printing."));
    }
    let count = pdfcraft_model::pages(&doc).len();
    let content = match job.text("content").unwrap_or("Document and comments") {
        "Document only" => Content::Document,
        "Document and stamps" => Content::DocumentAndStamps,
        "Form fields only" => Content::FormFieldsOnly,
        _ => Content::DocumentAndMarkups,
    };
    let subset = match job.text("sheets").unwrap_or("All pages") {
        "Odd pages only" => Subset::Odd,
        "Even pages only" => Subset::Even,
        _ => Subset::All,
    };
    let reverse = job.flag("reverse");
    let range = job.text("pages").ok().map(str::trim).filter(|s| !s.is_empty());
    let layout = match job.text("layout").unwrap_or("1 page per sheet") {
        "Booklet" => Layout::Booklet {
            subset: match subset {
                Subset::Odd => BookletSubset::FrontOnly,
                Subset::Even => BookletSubset::BackOnly,
                Subset::All => BookletSubset::BothSides,
            },
            binding: if job.text("binding").unwrap_or("Left") == "Right" { Binding::Right } else { Binding::Left },
        },
        "Poster" => Layout::Poster { scale: job.integer("poster_scale").unwrap_or(200).clamp(10, 2000) as f64, overlap: 18.0, cut_marks: job.flag("cut_marks") },
        "1 page per sheet" => Layout::default(),
        other => {
            let n: usize = other.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(1);
            match Layout::multiple(n) {
                Layout::Multiple { cols, rows, per_sheet, auto_rotate, .. } => {
                    Layout::Multiple { cols, rows, per_sheet, order: PageOrder::Horizontal, border: job.flag("border"), auto_rotate }
                }
                plain => plain,
            }
        }
    };
    // A booklet takes its pages whole: its fronts and backs are the subset.
    let pick = if matches!(layout, Layout::Booklet { .. }) { Subset::All } else { subset };
    let pages = pdfcraft_print::range::select_pages(count, range, &[], pick, reverse).map_err(|e| ReaderError::bad_request(pdfcraft_print::in_english(&e.to_string())))?;
    let paper = match job.text("paper").unwrap_or("Same as the pages") {
        "Same as the pages" => {
            let first = pdfcraft_model::pages(&doc).first().map(|p| p.display_size(&doc)).unwrap_or((595.28, 841.89));
            (first.0.min(first.1), first.0.max(first.1))
        }
        name => pdfcraft_print::PAPERS.iter().find(|(n, _)| *n == name).map(|(_, size)| *size).unwrap_or(pdfcraft_print::PAPERS[4].1),
    };
    let grayscale = job.flag("grayscale");
    let as_image = job.flag("as_image") || grayscale;
    if as_image {
        // Rendered first (the comments' choice applied), then imposed as images.
        pdfcraft_print::keep_printed(&mut doc, content);
        let filtered = write_full(&doc, &SaveOptions::default()).map_err(invalid)?;
        let dpi = job.integer("dpi").unwrap_or(300).clamp(72, 600) as f32;
        let mut renderer = PdfDocument::new(filtered).map_err(|e| invalid(e.message))?;
        let mut images = Vec::with_capacity(count);
        for ix in 0..count {
            job.check_cancelled()?;
            job.progress(ix as u64, count as u64 + 1, Some("Rendering".into()));
            let page = renderer.render_page_on(ix as u32, dpi / 72.0, Paper::Document)?;
            let png = if grayscale { to_grey(&page.png)? } else { page.png };
            images.push((format!("page-{ix}.png"), png));
        }
        doc = pdfcraft_create::from_images_with_resolution(&images, pdfcraft_create::ImageResolution::Dpi(dpi as f64)).map_err(invalid)?;
    }
    let settings = Settings {
        pages,
        paper,
        layout,
        // Already applied when the pages are images.
        content: if as_image { Content::Document } else { content },
        ..Settings::default()
    };
    job.progress(count as u64, count as u64 + 1, Some("Laying out".into()));
    let imposed = pdfcraft_print::impose(&doc, &settings).map_err(|e| invalid(pdfcraft_print::in_english(&e.to_string())))?;
    let sheets = Cos::open(Arc::new(imposed.clone())).map(|d| pdfcraft_model::pages(&d).len()).map_err(invalid)?;
    let stem = source.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
    let out = job.out_file(&format!("{stem} (print layout).pdf"))?;
    std::fs::write(&out, &imposed)?;
    Ok(ToolOutput {
        files: vec![out.to_string_lossy().into_owned()],
        notes: vec![format!("{sheets} sheets; check them in the tab, then print it (Ctrl+P)")],
        ..ToolOutput::default()
    })
}
