//! MimiReader round 19 (O12, T1 item 3): a PDF's own page numbers reach
//! the host — `i`, `ii`, `1` … — and a PDF numbered 1 … n sends none.

use mireader_reader_sdk::Reader as _;

/// Objects numbered from 1, a classic xref table and a trailer.
fn write_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (ix, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", ix + 1).into_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").into_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).into_bytes());
    out
}

/// `pages` blank pages; the catalog carries `labels` (a `/PageLabels`
/// dictionary) when given.
fn pdf(pages: usize, labels: Option<&str>) -> Vec<u8> {
    let kids: Vec<String> = (0..pages).map(|n| format!("{} 0 R", 3 + n)).collect();
    let catalog = match labels {
        Some(labels) => format!("<< /Type /Catalog /Pages 2 0 R /PageLabels {labels} >>"),
        None => "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
    };
    let mut objects = vec![catalog.into_bytes(), format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")).into_bytes()];
    for _ in 0..pages {
        objects.push(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>".to_vec());
    }
    write_pdf(&objects)
}

fn labels_of(bytes: Vec<u8>) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.pdf");
    std::fs::write(&path, bytes).unwrap();
    let doc = mireader_reader_pdf::PdfReader.open(&path).unwrap();
    doc.info().page_labels
}

#[test]
fn a_pdf_that_numbers_its_own_pages_says_how() {
    // Front matter in roman, the body from 1, an appendix as A-1.
    let labels = labels_of(pdf(6, Some("<< /Nums [0 << /S /r >> 2 << /S /D >> 5 << /S /D /P (A-) >>] >>")));
    assert_eq!(labels, ["i", "ii", "1", "2", "3", "A-1"]);
}

#[test]
fn a_pdf_numbered_one_to_n_sends_no_labels() {
    assert!(labels_of(pdf(3, None)).is_empty(), "no /PageLabels");
    assert!(labels_of(pdf(3, Some("<< /Nums [0 << /S /D >>] >>"))).is_empty(), "labels that are just 1 … n");
}
