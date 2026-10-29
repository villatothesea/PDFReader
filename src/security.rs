//! Round 20 (#198): security — passwords, permissions, redaction and
//! hidden information. Each tool writes a new file beside the original,
//! which is never changed; each checks what it wrote before keeping it.
//!
//! - **Protect with a password** — an open password and/or a permissions
//!   password, the permissions matrix, the encryption level.
//! - **Remove security** — once the permissions (owner) password is given.
//! - **Redact** — the marks already in the file, words, patterns (emails,
//!   phone numbers…) and whole pages are applied: the content under them
//!   is removed from the page streams (glyphs, image pixels, paths, form
//!   fields, annotations, tags — pdfcraft-redact), invisible text that
//!   could carry it is cleaned away, and the file is written whole, so no
//!   earlier revision keeps it. Then the copy is read again by hayro (the
//!   other implementation) and must not contain any redacted text.
//! - **Remove hidden information** — metadata, attachments, comments,
//!   hidden text and layers, links and scripts, private data; what went is
//!   said.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::sync::Arc;

use mireader_reader_sdk::{Document as _, ErrorCode, Job, ReaderError, ToolOutput};
use pdfcraft_annot::{Meta, NewAnnotation, OverlayLook, Shape, Style};
use pdfcraft_cos::{Algorithm, Document as Cos, NewEncryption, SaveOptions, write_full};
use pdfcraft_redact::patterns::Pattern;
use pdfcraft_redact::sanitize::Hidden;

use crate::PdfDocument;

pub const PROTECT: &str = "protect";
pub const UNPROTECT: &str = "remove-security";
pub const REDACT: &str = "redact";
pub const REMOVE_HIDDEN: &str = "remove-hidden";

pub fn handles(action: &str) -> bool {
    matches!(action, PROTECT | UNPROTECT | REDACT | REMOVE_HIDDEN)
}

pub fn run(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    match job.action {
        PROTECT => protect(job),
        UNPROTECT => unprotect(job),
        REDACT => redact(job),
        REMOVE_HIDDEN => remove_hidden(job),
        other => Err(ReaderError::bad_request(format!("no action {other:?}"))),
    }
}

/// The encryption a document has, in words (`None`: it has none).
pub(crate) fn method(doc: &Cos) -> Option<String> {
    let dict = doc.security()?.dict();
    Some(match dict.r {
        2 => "RC4, 40-bit".into(),
        3 => "RC4, 128-bit".into(),
        4 if dict.crypt_filters.iter().any(|(_, method)| *method == pdfcraft_crypt::Method::Aes128) => "AES, 128-bit".into(),
        4 => "RC4, 128-bit".into(),
        5 | 6 => "AES, 256-bit".into(),
        r => format!("revision {r}"),
    })
}

fn stem_of(path: &Path) -> String {
    path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into())
}

fn invalid(why: impl std::fmt::Display) -> ReaderError {
    ReaderError::invalid(why.to_string())
}

/// 32 bytes of entropy for keys and salts, from the OS-seeded hasher keys.
fn seed() -> [u8; 32] {
    let mut out = [0u8; 32];
    for (n, chunk) in out.chunks_mut(8).enumerate() {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_usize(n);
        hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    out
}

/// The source, opened by pdfcraft — with `password` when it has one.
fn open(path: &Path, password: Option<&str>) -> Result<Cos, ReaderError> {
    let bytes = std::fs::read(path)?;
    Cos::open_with_password(Arc::new(bytes), password).map_err(|error| match error {
        pdfcraft_cos::CosError::WrongPassword => ReaderError::new(ErrorCode::Encrypted, "that password doesn't open this PDF"),
        other => invalid(format!("It can't be read: {other}")),
    })
}

/// A non-empty password parameter.
fn password(job: &Job<'_>, id: &str) -> Option<String> {
    job.text(id).ok().filter(|p| !p.is_empty()).map(str::to_owned)
}

fn write(job: &Job<'_>, bytes: &[u8], name: &str) -> Result<String, ReaderError> {
    let out = job.out_file(name)?;
    std::fs::write(&out, bytes)?;
    Ok(out.to_string_lossy().into_owned())
}

// ---- protect / remove security ---------------------------------------------

fn protect(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let source = job.path().to_path_buf();
    let mut doc = open(&source, None)?;
    if doc.security().is_some() {
        return Err(ReaderError::new(ErrorCode::Unsupported, "This PDF is protected already. Remove its security first."));
    }
    let open_pw = password(job, "open_password");
    let owner_pw = password(job, "permissions_password");
    let Some(owner) = owner_pw.clone().or_else(|| open_pw.clone()) else {
        return Err(ReaderError::bad_request("Give a password to open it, a permissions password, or both."));
    };
    if open_pw.is_some() && open_pw == owner_pw {
        return Err(ReaderError::bad_request("The two passwords must differ: the permissions password is the one that lifts the restrictions."));
    }
    let algorithm = match job.text("encryption").unwrap_or("AES 256-bit") {
        "AES 128-bit" => Algorithm::Aes128,
        "RC4 128-bit" => Algorithm::Rc4_128,
        _ => Algorithm::Aes256,
    };
    // /P: bits from 1; 1, 2 and 7, 8 and 13–32 stay set as the format asks.
    let mut p: i32 = !0 & !((1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 8) | (1 << 9) | (1 << 10) | (1 << 11));
    let allow = |p: &mut i32, flag: &str, bits: &[u32]| {
        if job.flag(flag) {
            for bit in bits {
                *p |= 1 << (bit - 1);
            }
        }
    };
    allow(&mut p, "allow_print", &[3, 12]);
    allow(&mut p, "allow_changes", &[4]);
    allow(&mut p, "allow_copy", &[5, 10]);
    allow(&mut p, "allow_comments", &[6]);
    allow(&mut p, "allow_forms", &[9]);
    allow(&mut p, "allow_pages", &[11]);
    doc.set_encryption(&NewEncryption {
        algorithm,
        user_password: open_pw.as_deref().unwrap_or(""),
        owner_password: &owner,
        permissions: p,
        encrypt_metadata: !job.flag("leave_metadata"),
        seed: seed(),
    })
    .map_err(invalid)?;
    let bytes = write_full(&doc, &SaveOptions::default()).map_err(invalid)?;
    // Check: it opens with each password as it should.
    let check = |pw: Option<&str>| Cos::open_with_password(Arc::new(bytes.clone()), pw);
    if let Some(open_pw) = &open_pw {
        if check(None).is_ok() {
            return Err(invalid("The protected copy opened without its password, so it wasn't kept."));
        }
        check(Some(open_pw)).map_err(|_| invalid("The protected copy didn't open with its password, so it wasn't kept."))?;
    }
    let owned = check(Some(&owner)).map_err(|_| invalid("The protected copy didn't open with its permissions password, so it wasn't kept."))?;
    if owned.permissions().is_none_or(|p| !p.owner) {
        return Err(invalid("The permissions password didn't lift the restrictions, so the copy wasn't kept."));
    }
    let name = format!("{} (protected).pdf", stem_of(&source));
    let file = write(job, &bytes, &name)?;
    Ok(ToolOutput { files: vec![file], notes: vec![method(&owned).unwrap_or_default()], ..ToolOutput::default() })
}

fn unprotect(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let source = job.path().to_path_buf();
    let given = password(job, "password");
    let mut doc = open(&source, given.as_deref())?;
    if doc.security().is_none() {
        return Err(ReaderError::new(ErrorCode::Unsupported, "This PDF has no security to remove."));
    }
    // An open password alone doesn't lift the restrictions a permissions
    // password set.
    if doc.permissions().is_some_and(|p| !p.owner) {
        let restricted = doc.permissions().is_some_and(|p| !(p.print() && p.copy() && p.modify() && p.annotate() && p.assemble() && p.fill_forms()));
        if restricted {
            return Err(ReaderError::new(ErrorCode::Encrypted, "Give its permissions password: the password that opens it doesn't lift its restrictions."));
        }
    }
    doc.remove_encryption();
    let bytes = write_full(&doc, &SaveOptions::default()).map_err(invalid)?;
    let reread = Cos::open(Arc::new(bytes.clone())).map_err(|_| invalid("The copy didn't open without a password, so it wasn't kept."))?;
    if reread.security().is_some() {
        return Err(invalid("The copy still had its security, so it wasn't kept."));
    }
    let name = format!("{} (unprotected).pdf", stem_of(&source));
    Ok(ToolOutput { files: vec![write(job, &bytes, &name)?], ..ToolOutput::default() })
}

// ---- redaction -------------------------------------------------------------

/// The text ranges (in chars) of `page_text` to redact: each word or phrase
/// (case aside), and each chosen pattern.
fn matches(chars: &[char], words: &[String], patterns: &[Pattern]) -> Vec<std::ops::Range<usize>> {
    let lower: Vec<char> = chars.iter().flat_map(|c| c.to_lowercase()).collect();
    let mut out = Vec::new();
    // Lower-casing can change the count of chars; compare only where it
    // doesn't (it rarely does in running text).
    if lower.len() == chars.len() {
        for word in words {
            let needle: Vec<char> = word.chars().flat_map(|c| c.to_lowercase()).collect();
            if needle.is_empty() || needle.len() > lower.len() {
                continue;
            }
            let mut at = 0;
            while at + needle.len() <= lower.len() {
                if lower[at..at + needle.len()] == needle[..] {
                    out.push(at..at + needle.len());
                    at += needle.len();
                } else {
                    at += 1;
                }
            }
        }
    }
    for pattern in patterns {
        out.extend(pdfcraft_redact::patterns::find(*pattern, chars));
    }
    out
}

/// One quad per line the `range` of characters covers, in the text layer's
/// space.
fn quads(boxes: &[[f32; 4]], range: std::ops::Range<usize>) -> Vec<[f32; 8]> {
    let mut lines: Vec<[f32; 4]> = Vec::new();
    for b in boxes[range.start.min(boxes.len())..range.end.min(boxes.len())].iter().filter(|b| b[2] > b[0]) {
        match lines.last_mut() {
            // The same line: it overlaps the last box vertically.
            Some(line) if b[1] < line[3] && b[3] > line[1] && b[0] >= line[0] - 2.0 => {
                line[0] = line[0].min(b[0]);
                line[1] = line[1].min(b[1]);
                line[2] = line[2].max(b[2]);
                line[3] = line[3].max(b[3]);
            }
            _ => lines.push(*b),
        }
    }
    lines.into_iter().map(|[x0, y0, x1, y1]| [x0, y0, x1, y0, x0, y1, x1, y1]).collect()
}

fn redact(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let source = job.path().to_path_buf();
    let bytes = std::fs::read(&source)?;
    let mut doc = open(&source, None)?;
    if doc.permissions().is_some_and(|p| !p.modify()) {
        return Err(ReaderError::new(ErrorCode::Unsupported, "This PDF's security settings don't allow it to be changed, so it can't be redacted."));
    }
    let words: Vec<String> = job
        .text("words")
        .unwrap_or("")
        .split(['\n', ',', ';'])
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty())
        .collect();
    let patterns: Vec<Pattern> = [
        ("emails", Pattern::Email),
        ("phones", Pattern::Phone),
        ("cards", Pattern::CreditCard),
        ("ssns", Pattern::Ssn),
        ("dates", Pattern::Date),
    ]
    .into_iter()
    .filter(|(id, _)| job.flag(id))
    .map(|(_, p)| p)
    .collect();
    let count = pdfcraft_model::pages(&doc).len();
    let whole: Vec<usize> = match job.text("pages").ok().map(str::trim).filter(|s| !s.is_empty()) {
        Some(spec) => mireader_reader_sdk::parse_pages(spec, count)?,
        None => Vec::new(),
    };
    let fill = if job.text("fill").unwrap_or("Black") == "White" { [1.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0] };

    // Mark what was asked for, from the renderer's text layer.
    let mut renderer = PdfDocument::new(bytes).map_err(|error| invalid(error.message))?;
    let pages = pdfcraft_model::pages(&doc);
    let mut found: Vec<(usize, String)> = Vec::new();
    let mut names = 0;
    let mut meta = || {
        names += 1;
        Meta { date: None, id: format!("mimi-redact-{}-{names}", std::process::id()) }
    };
    for (ix, page) in pages.iter().enumerate() {
        job.check_cancelled()?;
        job.progress(ix as u64, count as u64 * 2, Some("Marking".into()));
        let mut marks: Vec<[f64; 8]> = Vec::new();
        if whole.contains(&ix) {
            let (w, h) = page.display_size(&doc);
            marks.push(user_quad(&doc, page, [0.0, 0.0, w as f32, 0.0, 0.0, h as f32, w as f32, h as f32]));
        } else if !words.is_empty() || !patterns.is_empty() {
            let text = renderer.page_text(ix as u32)?;
            let chars: Vec<char> = text.text.chars().collect();
            for range in matches(&chars, &words, &patterns) {
                found.push((ix, chars[range.clone()].iter().collect()));
                marks.extend(quads(&text.boxes, range).into_iter().map(|q| user_quad(&doc, page, q)));
            }
        }
        if !marks.is_empty() {
            let shape = Shape::Redact { quads: marks, overlay: String::new(), look: OverlayLook::default() };
            let mut style = Style::default_for(&shape);
            style.fill = Some(fill);
            let new = NewAnnotation { page: ix, shape, style, contents: String::new(), author: "MimiReader".into() };
            pdfcraft_annot::add_annotation(&mut doc, &new, &meta()).map_err(invalid)?;
        }
    }
    let marked = pdfcraft_redact::marks(&doc).len();
    if marked == 0 {
        return Err(ReaderError::bad_request("Nothing to redact: no marks in the file, and nothing found for the words, patterns or pages given."));
    }
    // Apply: the content under every mark leaves the page streams.
    let report = pdfcraft_redact::apply(&mut doc, None).map_err(invalid)?;
    // Residue: invisible text that may carry it.
    // (Nothing invisible to clean is no failure.)
    let residue = match pdfcraft_redact::sanitize::remove_hidden(&mut doc, &[Hidden::HiddenText]) {
        Ok(removed) => removed,
        Err(pdfcraft_redact::RedactError::NothingToApply) => Vec::new(),
        Err(error) => return Err(invalid(error)),
    };
    // Written whole: no earlier revision keeps what was removed.
    let clean = write_full(&doc, &SaveOptions::default()).map_err(invalid)?;
    // Verify with the other reader: nothing redacted can be read back.
    let mut check = PdfDocument::new(clean.clone()).map_err(|error| invalid(error.message))?;
    for (n, (ix, text)) in found.iter().enumerate() {
        job.progress((count + n) as u64, (count + found.len()) as u64, Some("Checking".into()));
        let after = check.page_text(*ix as u32)?.text.to_lowercase();
        if after.contains(&text.to_lowercase()) {
            return Err(invalid(format!("\"{text}\" could still be read on page {} after redacting, so the copy wasn't kept.", ix + 1)));
        }
    }
    if !pdfcraft_redact::marks(&Cos::open(Arc::new(clean.clone())).map_err(invalid)?).is_empty() {
        return Err(invalid("Redaction marks were left in the copy, so it wasn't kept."));
    }
    let name = format!("{} (redacted).pdf", stem_of(&source));
    let file = write(job, &clean, &name)?;
    let mut notes = vec![format!("{marked} marks applied on {} pages", report.pages)];
    if report.glyphs > 0 {
        notes.push(format!("{} characters removed", report.glyphs));
    }
    if report.images_removed + report.images_cleared > 0 {
        notes.push(format!("{} images removed or cleared", report.images_removed + report.images_cleared));
    }
    if report.annotations + report.fields > 0 {
        notes.push(format!("{} comments and fields removed", report.annotations + report.fields));
    }
    if residue.iter().any(|(_, n)| *n > 0) {
        notes.push("hidden text cleaned away".into());
    }
    notes.push("checked: no redacted text can be read".into());
    Ok(ToolOutput { files: vec![file], notes, ..ToolOutput::default() })
}

fn user_quad(doc: &Cos, page: &pdfcraft_model::Page, q: [f32; 8]) -> [f64; 8] {
    let mut out = [0.0; 8];
    for corner in 0..4 {
        let p = crate::edit::to_user(doc, page, q[corner * 2], q[corner * 2 + 1]);
        out[corner * 2] = p[0];
        out[corner * 2 + 1] = p[1];
    }
    out
}

// ---- hidden information ----------------------------------------------------

fn remove_hidden(job: &mut Job<'_>) -> Result<ToolOutput, ReaderError> {
    let source = job.path().to_path_buf();
    let mut doc = open(&source, None)?;
    if doc.permissions().is_some_and(|p| !p.modify()) {
        return Err(ReaderError::new(ErrorCode::Unsupported, "This PDF's security settings don't allow it to be changed."));
    }
    let which: Vec<Hidden> = [
        ("metadata", Hidden::Metadata),
        ("attachments", Hidden::Attachments),
        ("comments", Hidden::Comments),
        ("fields", Hidden::FormFields),
        ("hidden_text", Hidden::HiddenText),
        ("hidden_layers", Hidden::HiddenLayers),
        ("bookmarks", Hidden::Bookmarks),
        ("links", Hidden::LinksActionsScripts),
        ("private", Hidden::PrivateData),
    ]
    .into_iter()
    .filter(|(id, _)| job.flag(id))
    .map(|(_, h)| h)
    .collect();
    if which.is_empty() {
        return Err(ReaderError::bad_request("Choose at least one kind of information to remove."));
    }
    let removed = match pdfcraft_redact::sanitize::remove_hidden(&mut doc, &which) {
        Ok(removed) => removed,
        Err(pdfcraft_redact::RedactError::NothingToApply) => Vec::new(),
        Err(error) => return Err(invalid(error)),
    };
    let clean = write_full(&doc, &SaveOptions::default()).map_err(invalid)?;
    // Check: what was removed isn't found again.
    let reread = Cos::open(Arc::new(clean.clone())).map_err(invalid)?;
    let left: Vec<String> = pdfcraft_redact::sanitize::scan(&reread)
        .into_iter()
        .filter(|(kind, n)| *n > 0 && which.contains(kind))
        .map(|(kind, _)| kind.label().to_string())
        .collect();
    if !left.is_empty() {
        return Err(invalid(format!("Still found after removing: {}. The copy wasn't kept.", left.join(", "))));
    }
    let mut notes: Vec<String> = removed.iter().filter(|(_, n)| *n > 0).map(|(kind, n)| format!("{}: {n}", kind.label())).collect();
    if notes.is_empty() {
        notes.push("there was nothing of those kinds to remove".into());
    }
    let name = format!("{} (cleaned).pdf", stem_of(&source));
    Ok(ToolOutput { files: vec![write(job, &clean, &name)?], notes, ..ToolOutput::default() })
}
