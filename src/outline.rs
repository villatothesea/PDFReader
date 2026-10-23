//! The document outline ("bookmarks"): `/Outlines` in the catalog, a tree
//! of items linked by `/First` and `/Next`, each with a `/Title` and either
//! a `/Dest` or a `/GoTo` action. Destinations are explicit (`[page …]`) or
//! named (looked up in `/Dests` or the `/Names` → `/Dests` name tree).

use std::collections::{HashMap, HashSet};

use hayro::hayro_syntax::{
    Pdf,
    object::{Array, Dict, Name, Object, ObjectIdentifier, String as PdfString},
};
use mireader_reader_sdk::{OutlineEntry, Target};

use crate::decode_text;

/// Deepest nesting and most items read — a malformed file can't stall open.
const MAX_DEPTH: usize = 16;
const MAX_ITEMS: usize = 10_000;

pub fn read(pdf: &Pdf) -> Vec<OutlineEntry> {
    let xref = pdf.xref();
    let Some(catalog) = xref.get::<Dict<'_>>(xref.root_id()) else {
        return Vec::new();
    };
    let Some(outlines) = catalog.get::<Dict<'_>>(b"Outlines") else {
        return Vec::new();
    };
    let pages = page_ids(pdf);
    let mut walk = Walk {
        dests: Dests { catalog: &catalog, pages: &pages },
        seen: HashSet::new(),
        items: 0,
    };
    walk.children(&outlines, 0)
}

/// Each page object's index.
pub(crate) fn page_ids(pdf: &Pdf) -> HashMap<ObjectIdentifier, u32> {
    pdf.pages()
        .iter()
        .enumerate()
        .filter_map(|(ix, page)| Some((page.raw().obj_id()?, ix as u32)))
        .collect()
}

struct Walk<'w, 'a> {
    dests: Dests<'w, 'a>,
    /// Items already visited — a `/Next` cycle must not loop forever.
    seen: HashSet<ObjectIdentifier>,
    items: usize,
}

impl<'a> Walk<'_, 'a> {
    fn children(&mut self, parent: &Dict<'a>, depth: usize) -> Vec<OutlineEntry> {
        let mut out = Vec::new();
        if depth >= MAX_DEPTH {
            return out;
        }
        let mut next = parent.get::<Dict<'a>>(b"First");
        while let Some(item) = next {
            if let Some(id) = item.obj_id()
                && !self.seen.insert(id)
            {
                break;
            }
            self.items += 1;
            if self.items > MAX_ITEMS {
                break;
            }
            let title = item
                .get::<PdfString<'_>>(b"Title")
                .map(|title| decode_text(title.as_bytes()))
                .map(|title| title.trim().to_string())
                .unwrap_or_default();
            let children = self.children(&item, depth + 1);
            let page = self.dests.of(&item).map(|(page, _)| page);
            // An item with no usable target still groups its children; it
            // jumps to its first child's page.
            let target = page.or_else(|| children.first().map(|child| match child.target {
                Target::Page(page) | Target::Section(page) => page,
            }));
            if let Some(page) = target
                && !(title.is_empty() && children.is_empty())
            {
                out.push(OutlineEntry {
                    title,
                    target: Target::Page(page),
                    children,
                });
            }
            next = item.get::<Dict<'a>>(b"Next");
        }
        out
    }

}

/// Destinations — an outline item's or (round 14, #5) a link's: the page
/// and, when the destination says, the top of the view in user space.
pub(crate) struct Dests<'w, 'a> {
    pub catalog: &'w Dict<'a>,
    pub pages: &'w HashMap<ObjectIdentifier, u32>,
}

impl<'a> Dests<'_, 'a> {
    /// `item`'s `/Dest`, or its `/GoTo` action's `/D`.
    pub fn of(&self, item: &Dict<'a>) -> Option<(u32, Option<f64>)> {
        if let Some(dest) = item.get::<Object<'a>>(b"Dest") {
            return self.resolve(dest);
        }
        let action = item.get::<Dict<'a>>(b"A")?;
        if action.get::<Name<'_>>(b"S").is_some_and(|s| s.as_ref() == b"GoTo") {
            return self.resolve(action.get::<Object<'a>>(b"D")?);
        }
        None
    }

    /// An explicit destination array, or a name/string to look up.
    pub fn resolve(&self, dest: Object<'a>) -> Option<(u32, Option<f64>)> {
        self.resolve_at(dest, 0)
    }

    fn resolve_at(&self, dest: Object<'a>, depth: usize) -> Option<(u32, Option<f64>)> {
        if depth > MAX_DEPTH {
            return None;
        }
        match dest {
            Object::Array(array) => self.explicit(&array),
            Object::Name(name) => self.named(name.as_ref(), depth),
            Object::String(string) => self.named(string.as_bytes(), depth),
            // `/D` of a named destination's dictionary form.
            Object::Dict(dict) => self.resolve_at(dict.get::<Object<'a>>(b"D")?, depth + 1),
            _ => None,
        }
    }

    /// `[page /XYZ left top zoom]`, `[page /FitH top]`, `[page /FitBH top]`,
    /// `[page /FitR left bottom right top]` say where the view's top is;
    /// the other fits show the whole page.
    fn explicit(&self, array: &Array<'a>) -> Option<(u32, Option<f64>)> {
        let mut items = array.iter::<Object<'a>>();
        let page = match items.next()? {
            Object::Dict(page) => self.pages.get(&page.obj_id()?).copied()?,
            // Remote-style destinations number the page directly.
            Object::Number(number) => u32::try_from(number.as_i64()).ok()?,
            _ => return None,
        };
        let fit = match items.next() {
            Some(Object::Name(name)) => name.as_ref().to_vec(),
            _ => return Some((page, None)),
        };
        let rest: Vec<Option<f64>> = items
            .map(|item| match item {
                Object::Number(number) => Some(number.as_f64()),
                _ => None,
            })
            .collect();
        let top = match fit.as_slice() {
            b"XYZ" => rest.get(1).copied().flatten(),
            b"FitH" | b"FitBH" => rest.first().copied().flatten(),
            b"FitR" => rest.get(3).copied().flatten(),
            _ => None,
        };
        Some((page, top))
    }

    fn named(&self, name: &[u8], depth: usize) -> Option<(u32, Option<f64>)> {
        // PDF 1.1: /Dests dictionary in the catalog.
        if let Some(dests) = self.catalog.get::<Dict<'a>>(b"Dests")
            && let Some(dest) = dests.get::<Object<'a>>(name)
        {
            return self.resolve_at(dest, depth + 1);
        }
        // PDF 1.2+: the /Names → /Dests name tree.
        let tree = self.catalog.get::<Dict<'a>>(b"Names")?.get::<Dict<'a>>(b"Dests")?;
        let dest = name_tree_lookup(&tree, name, 0)?;
        self.resolve_at(dest, depth + 1)
    }
}

fn name_tree_lookup<'a>(node: &Dict<'a>, key: &[u8], depth: usize) -> Option<Object<'a>> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(names) = node.get::<Array<'a>>(b"Names") {
        let mut items = names.iter::<Object<'a>>();
        while let (Some(name), Some(value)) = (items.next(), items.next()) {
            let matches = match &name {
                Object::String(string) => string.as_bytes() == key,
                Object::Name(n) => n.as_ref() == key,
                _ => false,
            };
            if matches {
                return Some(value);
            }
        }
    }
    let kids = node.get::<Array<'a>>(b"Kids")?;
    for kid in kids.iter::<Dict<'a>>() {
        if let Some(found) = name_tree_lookup(&kid, key, depth + 1) {
            return Some(found);
        }
    }
    None
}
