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
    let pages: HashMap<ObjectIdentifier, u32> = pdf
        .pages()
        .iter()
        .enumerate()
        .filter_map(|(ix, page)| Some((page.raw().obj_id()?, ix as u32)))
        .collect();
    let mut walk = Walk {
        catalog: &catalog,
        pages: &pages,
        seen: HashSet::new(),
        items: 0,
    };
    walk.children(&outlines, 0)
}

struct Walk<'w, 'a> {
    catalog: &'w Dict<'a>,
    pages: &'w HashMap<ObjectIdentifier, u32>,
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
            let page = self.destination_page(&item);
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

    fn destination_page(&self, item: &Dict<'a>) -> Option<u32> {
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
    fn resolve(&self, dest: Object<'a>) -> Option<u32> {
        match dest {
            Object::Array(array) => self.explicit(&array),
            Object::Name(name) => self.named(name.as_ref()),
            Object::String(string) => self.named(string.as_bytes()),
            // `/D` of a named destination's dictionary form.
            Object::Dict(dict) => self.resolve(dict.get::<Object<'a>>(b"D")?),
            _ => None,
        }
    }

    fn explicit(&self, array: &Array<'a>) -> Option<u32> {
        let mut items = array.iter::<Object<'a>>();
        match items.next()? {
            Object::Dict(page) => self.pages.get(&page.obj_id()?).copied(),
            // Remote-style destinations number the page directly.
            Object::Number(number) => u32::try_from(number.as_i64()).ok(),
            _ => None,
        }
    }

    fn named(&self, name: &[u8]) -> Option<u32> {
        // PDF 1.1: /Dests dictionary in the catalog.
        if let Some(dests) = self.catalog.get::<Dict<'a>>(b"Dests")
            && let Some(dest) = dests.get::<Object<'a>>(name)
        {
            return self.resolve(dest);
        }
        // PDF 1.2+: the /Names → /Dests name tree.
        let tree = self.catalog.get::<Dict<'a>>(b"Names")?.get::<Dict<'a>>(b"Dests")?;
        let dest = name_tree_lookup(&tree, name, 0)?;
        self.resolve(dest)
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
