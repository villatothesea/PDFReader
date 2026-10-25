//! Item 19: a PDF edited in place — highlight, underline and strike-out over
//! text, sticky notes, recolouring and deleting comments, bookmarks — with
//! undo, and saved as an incremental update (the original bytes, and any
//! signature over them, stay untouched) after a check that the file hasn't
//! changed on disk. Built on pdfcraft's object layer (D53).
//!
//! The host speaks in the text layer's space (page points, top-left
//! origin, after rotation); comments live in the page's user space. Each
//! page's view matrix maps between the two.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use mireader_reader_sdk::{
    Annotation, AnnotationKind, EditGuard, EditInfo, EditKind, EditOp, EditState, ErrorCode, GuardKind, MarkupKind,
    ReaderError,
};
use pdfcraft_annot::{Markup, Meta, NewAnnotation, NoteIcon, Shape, Style};
use pdfcraft_cos::{Dict, Document as Cos, Object, SaveOptions, write_incremental};
use pdfcraft_model::Page;

/// The annotation flag that forbids changing a comment (ISO 32000-2 §12.5.3).
const LOCKED: i64 = 1 << 7;
const AUTHOR: &str = "MimiReader";

/// The file as last read or written; a save refuses to overwrite anything
/// else.
#[derive(Debug, Clone, PartialEq)]
struct OnDisk {
    len: u64,
    modified: Option<SystemTime>,
}

impl OnDisk {
    fn of(path: &Path) -> Option<OnDisk> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(OnDisk { len: metadata.len(), modified: metadata.modified().ok() })
    }
}

/// One version of the document, numbered so `dirty` is a comparison.
struct Version {
    doc: Cos,
    id: u64,
}

pub(crate) struct Editor {
    path: PathBuf,
    current: Version,
    undo: Vec<Version>,
    redo: Vec<Version>,
    next_id: u64,
    saved_id: u64,
    /// The bytes on disk when opened or last saved (for the conflict check
    /// when length and time say the file changed but its bytes didn't).
    disk_bytes: Arc<Vec<u8>>,
    disk: Option<OnDisk>,
    kinds: Vec<EditKind>,
    guard: Option<EditGuard>,
    names: u64,
}

fn io(error: impl std::fmt::Display) -> ReaderError {
    ReaderError::new(ErrorCode::Io, error.to_string())
}

fn failed(error: impl std::fmt::Display) -> ReaderError {
    ReaderError::bad_request(error.to_string())
}

/// `[x, y]` in the text layer's space → the page's user space.
fn to_user(doc: &Cos, page: &Page, x: f32, y: f32) -> [f64; 2] {
    let (_, h) = page.display_size(doc);
    let m = page.view_matrix(doc);
    let (dx, dy) = (x as f64, h - y as f64);
    [m[0] * dx + m[2] * dy + m[4], m[1] * dx + m[3] * dy + m[5]]
}

/// The page's user space → the text layer's space.
fn to_display(doc: &Cos, page: &Page, x: f64, y: f64) -> [f32; 2] {
    let (_, h) = page.display_size(doc);
    let m = page.view_matrix(doc);
    let det = m[0] * m[3] - m[1] * m[2];
    let (px, py) = (x - m[4], y - m[5]);
    let (dx, dy) = if det.abs() < f64::EPSILON {
        (px, py)
    } else {
        ((m[3] * px - m[2] * py) / det, (-m[1] * px + m[0] * py) / det)
    };
    [dx as f32, (h - dy) as f32]
}

fn numbers(doc: &Cos, object: Option<&Object>) -> Vec<f64> {
    object
        .map(|o| doc.resolve(o))
        .and_then(|o| o.as_array().map(|items| items.iter().filter_map(|x| doc.resolve(x).as_f64()).collect()))
        .unwrap_or_default()
}

fn text(doc: &Cos, object: Option<&Object>) -> Option<String> {
    let resolved = doc.resolve(object?);
    let s = resolved.as_string()?.to_text();
    (!s.is_empty()).then_some(s)
}

fn kind_of(subtype: &[u8]) -> AnnotationKind {
    match subtype {
        b"Highlight" => AnnotationKind::Highlight,
        b"Underline" => AnnotationKind::Underline,
        b"StrikeOut" => AnnotationKind::StrikeOut,
        b"Text" => AnnotationKind::Note,
        other => AnnotationKind::Other(String::from_utf8_lossy(other).into_owned()),
    }
}

/// The annotation's id: its `/NM` when it has one, else its object number.
fn id_of(doc: &Cos, entry: &Object, dict: &Dict, page: usize, index: usize) -> String {
    if let Some(name) = text(doc, dict.get(b"NM")) {
        return name;
    }
    match entry.as_ref() {
        Some(r) => format!("obj:{}", r.num),
        None => format!("at:{page}:{index}"),
    }
}

fn annots_of(doc: &Cos, page: &Page) -> Vec<Object> {
    page.dict
        .get(b"Annots")
        .map(|a| doc.resolve(a))
        .and_then(|a| a.as_array().cloned())
        .unwrap_or_default()
}

/// Is the document signed, and does a certification forbid comments?
/// (`Some(true)`: signed, comments refused.)
fn signature(doc: &Cos) -> Option<bool> {
    let root = doc.dict(&Object::Ref(doc.root()?))?;
    // A certification (DocMDP) with P = 1 or 2 allows no comments.
    let certified_strict = root
        .get(b"Perms")
        .and_then(|p| doc.dict(p))
        .and_then(|perms| perms.get(b"DocMDP").cloned())
        .and_then(|sig| doc.dict(&sig))
        .and_then(|sig| sig.get(b"Reference").map(|r| doc.resolve(r)))
        .and_then(|refs| refs.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| doc.dict(&r))
        .and_then(|r| r.get(b"TransformParams").and_then(|t| doc.dict(t)))
        .and_then(|t| t.int(b"P"))
        .is_some_and(|p| p < 3);
    let form = root.get(b"AcroForm").and_then(|f| doc.dict(f))?;
    let fields = form.get(b"Fields").map(|f| doc.resolve(f))?;
    let mut stack: Vec<Object> = fields.as_array().cloned().unwrap_or_default();
    let mut seen = 0;
    while let Some(field) = stack.pop() {
        seen += 1;
        if seen > 10_000 {
            break;
        }
        let Some(dict) = doc.dict(&field) else { continue };
        if dict.name(b"FT") == Some(b"Sig") && dict.get(b"V").is_some() {
            return Some(certified_strict);
        }
        if let Some(kids) = dict.get(b"Kids").map(|k| doc.resolve(k)) {
            stack.extend(kids.as_array().cloned().unwrap_or_default());
        }
    }
    None
}

impl Editor {
    /// An editor over `bytes` (the file at `path`); `None` when pdfcraft
    /// can't read the file well enough to write it back.
    pub(crate) fn open(path: &Path, bytes: Arc<Vec<u8>>, password: Option<&str>) -> Option<Editor> {
        let doc = Cos::open_with_password(bytes.clone(), password).ok()?;
        // Round 19 (T1 item 6): and the Info dictionary's four fields.
        let mut kinds = vec![EditKind::Markup, EditKind::Note, EditKind::Bookmark, EditKind::Info];
        let mut guard = None;
        if let Some(permissions) = doc.permissions()
            && !(permissions.annotate() && permissions.modify())
        {
            kinds.clear();
            guard = Some(EditGuard {
                kind: GuardKind::Permissions,
                message: "This PDF's security settings don't allow comments or changes.".into(),
            });
        }
        match signature(&doc) {
            Some(true) if guard.is_none() => {
                kinds.clear();
                guard = Some(EditGuard {
                    kind: GuardKind::Permissions,
                    message: "This PDF is certified so that it can't be changed.".into(),
                });
            }
            Some(_) if guard.is_none() => {
                guard = Some(EditGuard {
                    kind: GuardKind::Signed,
                    message: "This PDF is signed. Comments are saved as a new revision after the signature; \
                              some viewers then show the signature as changed."
                        .into(),
                });
            }
            _ => {}
        }
        if guard.is_none() && doc.security().is_some() {
            guard = Some(EditGuard {
                kind: GuardKind::Encrypted,
                message: "This PDF is password-protected; comments are saved with the same protection.".into(),
            });
        }
        Some(Editor {
            path: path.to_path_buf(),
            current: Version { doc, id: 0 },
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved_id: 0,
            disk_bytes: bytes,
            disk: OnDisk::of(path),
            kinds,
            guard,
            names: 0,
        })
    }

    pub(crate) fn state(&self) -> EditState {
        EditState {
            dirty: self.current.id != self.saved_id,
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }

    /// MimiReader round 19 (O12, T1 item 6): the document as it stands now
    /// (its properties are read from it).
    pub(crate) fn document(&self) -> &Cos {
        &self.current.doc
    }

    pub(crate) fn info(&self) -> EditInfo {
        EditInfo { kinds: self.kinds.clone(), guard: self.guard.clone(), state: self.state() }
    }

    /// Round 19 (O12, T1 item 3): the document's own page numbers (`/PageLabels`),
    /// one per page — `None` when its label tree can't be read.
    pub(crate) fn page_labels(&self) -> Option<Vec<String>> {
        pdfcraft_organize::page_labels(&self.current.doc).ok()
    }

    /// The document as it stands, as PDF bytes (what the renderer reads).
    pub(crate) fn bytes(&self) -> Result<Vec<u8>, ReaderError> {
        if !self.current.doc.is_modified() {
            return Ok(self.current.doc.bytes().as_ref().clone());
        }
        write_incremental(&self.current.doc, &SaveOptions::default()).map_err(failed)
    }

    fn pages(&self) -> Vec<Page> {
        pdfcraft_model::pages(&self.current.doc)
    }

    pub(crate) fn annotations(&self, page_ix: u32) -> Result<Vec<Annotation>, ReaderError> {
        let doc = &self.current.doc;
        let pages = self.pages();
        let page = pages
            .get(page_ix as usize)
            .ok_or_else(|| ReaderError::bad_request(format!("no page {page_ix}")))?;
        let mut out = Vec::new();
        for (index, entry) in annots_of(doc, page).iter().enumerate() {
            let Some(dict) = doc.dict(entry) else { continue };
            let subtype = dict.name(b"Subtype").unwrap_or_default().to_vec();
            if matches!(subtype.as_slice(), b"Popup" | b"Link" | b"Widget") {
                continue;
            }
            let rect = numbers(doc, dict.get(b"Rect"));
            if rect.len() != 4 {
                continue;
            }
            let corners = [
                to_display(doc, page, rect[0], rect[1]),
                to_display(doc, page, rect[2], rect[3]),
                to_display(doc, page, rect[0], rect[3]),
                to_display(doc, page, rect[2], rect[1]),
            ];
            let display = [
                corners.iter().map(|c| c[0]).fold(f32::MAX, f32::min),
                corners.iter().map(|c| c[1]).fold(f32::MAX, f32::min),
                corners.iter().map(|c| c[0]).fold(f32::MIN, f32::max),
                corners.iter().map(|c| c[1]).fold(f32::MIN, f32::max),
            ];
            let points = numbers(doc, dict.get(b"QuadPoints"));
            let quads = points
                .chunks_exact(8)
                .map(|q| {
                    let mut quad = [0.0f32; 8];
                    for corner in 0..4 {
                        let p = to_display(doc, page, q[corner * 2], q[corner * 2 + 1]);
                        quad[corner * 2] = p[0];
                        quad[corner * 2 + 1] = p[1];
                    }
                    quad
                })
                .collect();
            let color = numbers(doc, dict.get(b"C"));
            let kind = kind_of(&subtype);
            let locked = dict.int(b"F").is_some_and(|flags| flags & LOCKED != 0);
            let editable = !locked
                && !self.kinds.is_empty()
                && matches!(
                    kind,
                    AnnotationKind::Highlight | AnnotationKind::Underline | AnnotationKind::StrikeOut | AnnotationKind::Note
                );
            out.push(Annotation {
                id: id_of(doc, entry, &dict, page_ix as usize, index),
                page: page_ix,
                kind,
                rect: display,
                quads,
                color: (color.len() == 3).then(|| [color[0] as f32, color[1] as f32, color[2] as f32]),
                contents: text(doc, dict.get(b"Contents")),
                author: text(doc, dict.get(b"T")),
                editable,
            });
        }
        Ok(out)
    }

    /// The page and `/Annots` index of comment `id`.
    fn find(&self, id: &str) -> Result<(usize, usize), ReaderError> {
        let doc = &self.current.doc;
        for (page_ix, page) in self.pages().iter().enumerate() {
            for (index, entry) in annots_of(doc, page).iter().enumerate() {
                if let Some(dict) = doc.dict(entry)
                    && id_of(doc, entry, &dict, page_ix, index) == id
                {
                    return Ok((page_ix, index));
                }
            }
        }
        Err(ReaderError::bad_request(format!("no comment {id}")))
    }

    fn meta(&mut self) -> Meta {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        self.names += 1;
        Meta {
            date: Some(pdfcraft_cos::pdf_date(now)),
            id: format!("mimi-{now:x}-{}-{}", std::process::id(), self.names),
        }
    }

    /// Apply `op`; returns the id of a comment it made.
    pub(crate) fn apply(&mut self, op: EditOp) -> Result<Option<String>, ReaderError> {
        if let Some(kind) = op.kind()
            && !self.kinds.contains(&kind)
        {
            return Err(ReaderError::unsupported("this PDF can't be changed"));
        }
        if op.kind().is_none() && self.kinds.is_empty() {
            return Err(ReaderError::unsupported("this PDF can't be changed"));
        }
        let mut doc = self.current.doc.clone();
        let created = self.apply_to(&mut doc, op)?;
        self.undo.push(std::mem::replace(&mut self.current, Version { doc, id: self.next_id }));
        self.next_id += 1;
        self.redo.clear();
        Ok(created)
    }

    fn apply_to(&mut self, doc: &mut Cos, op: EditOp) -> Result<Option<String>, ReaderError> {
        let pages = pdfcraft_model::pages(doc);
        let page = |ix: u32| {
            pages
                .get(ix as usize)
                .cloned()
                .ok_or_else(|| ReaderError::bad_request(format!("no page {ix}")))
        };
        match op {
            EditOp::AddMarkup { page: ix, kind, quads, color, contents } => {
                let geometry = page(ix)?;
                let quads: Vec<[f64; 8]> = quads
                    .iter()
                    .map(|q| {
                        let mut out = [0.0; 8];
                        for corner in 0..4 {
                            let p = to_user(doc, &geometry, q[corner * 2], q[corner * 2 + 1]);
                            out[corner * 2] = p[0];
                            out[corner * 2 + 1] = p[1];
                        }
                        out
                    })
                    .collect();
                if quads.is_empty() {
                    return Err(ReaderError::bad_request("a markup needs some text to cover"));
                }
                let kind = match kind {
                    MarkupKind::Highlight => Markup::Highlight,
                    MarkupKind::Underline => Markup::Underline,
                    MarkupKind::StrikeOut => Markup::StrikeOut,
                };
                let shape = Shape::TextMarkup { kind, quads };
                let mut style = Style::default_for(&shape);
                if let Some(c) = color {
                    style.color = [c[0] as f64, c[1] as f64, c[2] as f64];
                }
                let meta = self.meta();
                let new = NewAnnotation {
                    page: ix as usize,
                    shape,
                    style,
                    contents: contents.unwrap_or_default(),
                    author: AUTHOR.into(),
                };
                pdfcraft_annot::add_annotation(doc, &new, &meta).map_err(failed)?;
                Ok(Some(meta.id))
            }
            EditOp::AddNote { page: ix, at, contents, color } => {
                let geometry = page(ix)?;
                let at = to_user(doc, &geometry, at[0], at[1]);
                let shape = Shape::Note { at, icon: NoteIcon::Comment };
                let mut style = Style::default_for(&shape);
                if let Some(c) = color {
                    style.color = [c[0] as f64, c[1] as f64, c[2] as f64];
                }
                let meta = self.meta();
                let new = NewAnnotation { page: ix as usize, shape, style, contents, author: AUTHOR.into() };
                pdfcraft_annot::add_annotation(doc, &new, &meta).map_err(failed)?;
                Ok(Some(meta.id))
            }
            EditOp::SetColor { id, color } => {
                let (page_ix, index) = self.find(&id)?;
                let meta = self.meta();
                let color = [color[0] as f64, color[1] as f64, color[2] as f64];
                pdfcraft_annot::set_style(doc, page_ix, index, Some(color), None, None, None, &meta).map_err(failed)?;
                Ok(None)
            }
            EditOp::SetContents { id, contents } => {
                let (page_ix, index) = self.find(&id)?;
                let meta = self.meta();
                pdfcraft_annot::set_contents(doc, page_ix, index, &contents, &meta).map_err(failed)?;
                Ok(None)
            }
            EditOp::Delete { id } => {
                let (page_ix, index) = self.find(&id)?;
                pdfcraft_annot::delete_annotation(doc, page_ix, index).map_err(failed)?;
                Ok(None)
            }
            EditOp::AddBookmark { parent, index, title, page } => {
                pdfcraft_organize::add_bookmark(doc, &parent, index, &title, page as usize).map_err(failed)?;
                Ok(None)
            }
            EditOp::RenameBookmark { path, title } => {
                pdfcraft_organize::rename_bookmark(doc, &path, &title).map_err(failed)?;
                Ok(None)
            }
            EditOp::MoveBookmarkPage { path, page } => {
                pdfcraft_organize::set_bookmark_page(doc, &path, page as usize).map_err(failed)?;
                Ok(None)
            }
            EditOp::DeleteBookmark { path } => {
                pdfcraft_organize::delete_bookmark(doc, &path).map_err(failed)?;
                Ok(None)
            }
            EditOp::SetInfo { title, author, subject, keywords } => {
                set_info(doc, [("Title", title), ("Author", author), ("Subject", subject), ("Keywords", keywords)])?;
                Ok(None)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn current_doc(&self) -> &Cos {
        &self.current.doc
    }

    pub(crate) fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.current, previous));
        true
    }

    pub(crate) fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.current, next));
        true
    }

    /// Has the file on disk changed since it was opened or last saved?
    fn changed_on_disk(&self) -> bool {
        let now = OnDisk::of(&self.path);
        if now == self.disk {
            return false;
        }
        // Length or time moved: compare the bytes (a touch isn't a change).
        std::fs::read(&self.path).map_or(true, |bytes| bytes != *self.disk_bytes)
    }

    /// Write the edits to the file: an incremental update (a full rewrite
    /// when encrypting needs one), into a temporary file beside it, then
    /// swapped in. Refuses (`Conflict`) when the file changed on disk.
    pub(crate) fn save(&mut self) -> Result<(), ReaderError> {
        if self.changed_on_disk() {
            return Err(ReaderError::new(
                ErrorCode::Conflict,
                "The PDF changed on disk since it was opened. Reopen it to see the other changes; yours were not written.",
            ));
        }
        let path = self.path.clone();
        self.write_to(&path)
    }

    pub(crate) fn save_as(&mut self, path: &Path) -> Result<(), ReaderError> {
        self.write_to(path)?;
        self.path = path.to_path_buf();
        Ok(())
    }

    fn write_to(&mut self, path: &Path) -> Result<(), ReaderError> {
        let bytes = self.bytes()?;
        let folder = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // The reader's file tree hides `.mireader-save-*` while it exists.
        let temp = folder.join(format!(".mireader-save-{name}.tmp"));
        std::fs::write(&temp, &bytes).map_err(io)?;
        if let Err(error) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(ReaderError::new(
                ErrorCode::Io,
                format!("The PDF couldn't be replaced — is it open in another program that locks it? ({error})"),
            ));
        }
        // The saved bytes are the new base: later edits append to them.
        let bytes = Arc::new(bytes);
        let reopened = self.current.doc.open_with_same_credentials(bytes.clone()).map_err(failed)?;
        self.current.doc = reopened;
        self.saved_id = self.current.id;
        self.disk_bytes = bytes;
        self.disk = OnDisk::of(path);
        Ok(())
    }
}

/// Round 19 (T1 item 6): the Info dictionary's descriptive fields, set (an
/// empty one removed). An indirect Info is updated in place, so the saved
/// update carries just that object; a missing or direct one becomes a new
/// object the trailer points to. The writer stamps ModDate.
fn set_info(doc: &mut Cos, fields: [(&str, String); 4]) -> Result<(), ReaderError> {
    let apply = |info: &mut Dict| {
        for (key, value) in &fields {
            let value = value.trim();
            if value.is_empty() {
                info.remove(key.as_bytes());
            } else {
                info.set(*key, Object::String(pdfcraft_cos::PdfString::text(value)));
            }
        }
    };
    match doc.trailer().get(b"Info").cloned() {
        Some(Object::Ref(info)) => doc.update_dict(info, apply).map_err(failed),
        existing => {
            let mut info = match existing {
                Some(Object::Dict(dict)) => dict,
                _ => Dict::new(),
            };
            apply(&mut info);
            let info = doc.add(Object::Dict(info));
            doc.trailer_mut().set("Info", Object::Ref(info));
            Ok(())
        }
    }
}