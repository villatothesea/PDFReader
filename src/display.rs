//! Round 19 (the corpus sweep, P2): the document the pages are drawn from.
//!
//! A form field or annotation without a normal appearance (`/AP /N`), or a
//! form that sets `/NeedAppearances`, leaves the drawing to the viewer.
//! hayro draws only appearances, so those fields and comments were missing
//! from the page and from its text (a filled-in form showed empty boxes).
//! The pages are now drawn from a copy with the missing appearances added,
//! as pdfcraft draws them when one is typed or placed: text and choice
//! fields from their values (under /NeedAppearances a field with a value is
//! redrawn even if it has a look; one without keeps the look it has),
//! check boxes and radio buttons as a drawn mark,
//! comments from their own dictionary. The copy is the file's bytes plus an
//! incremental update, made in memory; the file on disk and the editor
//! (saves, undo) keep the original.

use std::sync::Arc;

use pdfcraft_cos::{Dict, Document as Cos, Object, SaveOptions, write_incremental};
use pdfcraft_forms::FieldKind;

/// Annotations whose look the viewer never draws from the dictionary.
fn needs_no_appearance(subtype: &[u8]) -> bool {
    matches!(subtype, b"Link" | b"Popup" | b"Widget")
}

/// The bytes to draw `bytes` from: `None` when nothing is missing (or the
/// file can't be read this way), so the file itself is drawn.
pub(crate) fn with_appearances(bytes: &[u8]) -> Option<Vec<u8>> {
    let doc = Cos::open(Arc::new(bytes.to_vec())).ok()?;
    // An encrypted file's update would have to be encrypted too; it is drawn
    // as it is.
    if doc.permissions().is_some() {
        return None;
    }
    let mut copy: Option<Cos> = None;
    let drawn = |doc: &Cos, d: &Dict| d.get(b"AP").and_then(|ap| doc.resolve(ap).as_dict().map(|ap| ap.contains(b"N"))).unwrap_or(false);
    let redraw_all = pdfcraft_forms::need_appearances(&doc);

    for field in pdfcraft_forms::fields(&doc) {
        if matches!(field.kind, FieldKind::Text | FieldKind::Combo | FieldKind::List) {
            // A widget without an appearance is drawn from the value. Under
            // /NeedAppearances one with an appearance is redrawn only when
            // the field has a value: a field with none keeps the look it
            // has, which can be a calculated result (text_field_own_canvas_calc
            // went blank when it was redrawn from nothing).
            let has_value = field.value.iter().any(|v| !v.is_empty());
            for widget in &field.widgets {
                let Some(d) = doc.get(widget.obj).as_dict().cloned() else { continue };
                if drawn(&doc, &d) && !(redraw_all && has_value) {
                    continue;
                }
                let (stream, _) = pdfcraft_forms::appearance::field_appearance_drawn(&doc, &field, widget, &field.value, true);
                let target = copy.get_or_insert_with(|| doc.clone());
                let ap = target.add(Object::Stream(stream));
                let mut apd = d.get(b"AP").and_then(|ap| doc.resolve(ap).as_dict().cloned()).unwrap_or_default();
                apd.set(b"N".to_vec(), Object::Ref(ap));
                // Fails only when the widget isn't a dictionary; it then keeps its look.
                target.update_dict(widget.obj, |d| d.set(b"AP".to_vec(), Object::Dict(apd))).ok();
            }
            continue;
        }
        if !matches!(field.kind, FieldKind::CheckBox | FieldKind::Radio) {
            continue;
        }
        for widget in &field.widgets {
            let Some(d) = doc.get(widget.obj).as_dict().cloned() else { continue };
            if drawn(&doc, &d) {
                continue;
            }
            let on = widget.on_state.clone().unwrap_or_else(|| "Yes".to_owned());
            let checked = field.value.iter().any(|v| *v == on) || widget.state.as_deref() == Some(on.as_str());
            let target = copy.get_or_insert_with(|| doc.clone());
            let ap = pdfcraft_forms::appearance::check_box_states(target, widget, field.kind, &on);
            let state = if checked { on.clone() } else { "Off".to_owned() };
            // Fails only when the widget isn't a dictionary; it then stays undrawn.
            target
                .update_dict(widget.obj, |d| {
                    d.set(b"AP".to_vec(), Object::Dict(ap));
                    d.set(b"AS".to_vec(), Object::name(&state));
                })
                .ok();
        }
    }

    for page in pdfcraft_model::pages(&doc) {
        let Some(annots) = page.dict.get(b"Annots").map(|a| doc.resolve(a)) else { continue };
        let Some(annots) = annots.as_array() else { continue };
        for annot in annots {
            let Some(r) = annot.as_ref() else { continue };
            let Some(d) = doc.get(r).as_dict().cloned() else { continue };
            if d.name(b"Subtype").is_none_or(needs_no_appearance) || drawn(&doc, &d) {
                continue;
            }
            let Some(stream) = pdfcraft_annot::appearance::build(&d) else { continue };
            let target = copy.get_or_insert_with(|| doc.clone());
            let ap = target.add(Object::Stream(stream));
            let mut apd = Dict::new();
            apd.set(b"N".to_vec(), Object::Ref(ap));
            target.update_dict(r, |d| d.set(b"AP".to_vec(), Object::Dict(apd))).ok();
        }
    }

    write_incremental(&copy?, &SaveOptions::default()).ok()
}
