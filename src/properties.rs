//! MimiReader round 19 (O12, T1 item 6): a PDF's properties — the facts
//! behind the host's Document Properties dialog, read through pdfcraft's
//! object layer. Facts only; the host words them.
//!
//! - **Fonts** are what the dialog leans on ("3 of 5 fonts aren't embedded"
//!   predicts how the file looks elsewhere), so they are counted carefully:
//!   every page's resources and the forms inside them, each font once by
//!   name (a subset's `ABCDEF+` prefix removed), embedded only if every use
//!   of it carries its glyphs, and the 14 standard PDF fonts marked as such
//!   — every viewer has them, so not embedding them is fine.

use std::collections::{BTreeMap, HashSet};

use mireader_reader_sdk::{DocumentProperties, FontInfo, Permissions};
use pdfcraft_cos::{Dict, Document as Cos, Object};

/// The 14 fonts every PDF viewer carries (ISO 32000-1 §9.6.2.2).
const STANDARD_14: [&str; 14] = [
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
    "Symbol",
    "ZapfDingbats",
];

/// How deep forms nest inside forms before the walk stops (a malformed file
/// can loop).
const MAX_FORM_DEPTH: usize = 8;

/// A font's name without a subset's `ABCDEF+` prefix.
fn without_subset(name: &str) -> &str {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

fn text(doc: &Cos, dict: &Dict, key: &[u8]) -> Option<String> {
    let value = doc.resolve(dict.get(key)?);
    let s = value.as_string()?.to_text();
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// `D:YYYYMMDDHHmmSS…` as `YYYY-MM-DD` (the dialog says "on" a day).
fn date(raw: &str) -> Option<String> {
    let digits: String = raw.trim().trim_start_matches("D:").chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    let (y, m, d) = (&digits[0..4], &digits[4..6], &digits[6..8]);
    let (mm, dd): (u32, u32) = (m.parse().ok()?, d.parse().ok()?);
    ((1..=12).contains(&mm) && (1..=31).contains(&dd)).then(|| format!("{y}-{m}-{d}"))
}

/// Is this font's program in the file?
fn embedded(doc: &Cos, font: &Dict) -> bool {
    if font.name(b"Subtype") == Some(b"Type3") {
        // Its glyphs are drawn by procedures inside the file.
        return true;
    }
    let descriptor_of = |font: &Dict| font.get(b"FontDescriptor").and_then(|d| doc.dict(d));
    let descriptor = if font.name(b"Subtype") == Some(b"Type0") {
        font.get(b"DescendantFonts")
            .map(|d| doc.resolve(d))
            .and_then(|d| d.as_array().and_then(|a| a.first().cloned()))
            .and_then(|d| doc.dict(&d))
            .and_then(|d| descriptor_of(&d))
    } else {
        descriptor_of(font)
    };
    descriptor.is_some_and(|d| [b"FontFile".as_slice(), b"FontFile2", b"FontFile3"].iter().any(|key| d.get(key).is_some()))
}

/// Every font the pages use: name → (embedded everywhere, standard).
struct Fonts<'a> {
    doc: &'a Cos,
    found: BTreeMap<String, (bool, bool)>,
    /// Resource dictionaries and fonts already looked at (by object number).
    seen: HashSet<(u32, u16)>,
}

impl Fonts<'_> {
    fn resources(&mut self, resources: &Object, depth: usize) {
        if let Some(r) = resources.as_ref()
            && !self.seen.insert((r.num, r.generation))
        {
            return;
        }
        let Some(resources) = self.doc.dict(resources) else { return };
        if let Some(fonts) = resources.get(b"Font").and_then(|f| self.doc.dict(f)) {
            for (_, font) in fonts.iter() {
                self.font(font);
            }
        }
        if depth >= MAX_FORM_DEPTH {
            return;
        }
        if let Some(xobjects) = resources.get(b"XObject").and_then(|x| self.doc.dict(x)) {
            for (_, xobject) in xobjects.iter() {
                if let Some(r) = xobject.as_ref()
                    && !self.seen.insert((r.num, r.generation))
                {
                    continue;
                }
                let Some(form) = self.doc.dict(xobject) else { continue };
                if form.name(b"Subtype") == Some(b"Form")
                    && let Some(inner) = form.get(b"Resources")
                {
                    self.resources(inner, depth + 1);
                }
            }
        }
    }

    fn font(&mut self, font: &Object) {
        let Some(dict) = self.doc.dict(font) else { return };
        let raw = dict.name(b"BaseFont").map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default();
        let name = if raw.is_empty() { "(unnamed)".to_string() } else { without_subset(&raw).to_string() };
        let is_embedded = embedded(self.doc, &dict);
        let standard = !is_embedded && STANDARD_14.contains(&name.as_str());
        let entry = self.found.entry(name).or_insert((true, standard));
        // Embedded only if every use carries its glyphs.
        entry.0 &= is_embedded;
        entry.1 &= standard || is_embedded;
    }
}

/// The names in a name tree (attachments), up to a bound.
fn name_tree(doc: &Cos, node: &Object, out: &mut Vec<String>, budget: &mut usize) {
    let Some(node) = doc.dict(node) else { return };
    if let Some(names) = node.get(b"Names").map(|n| doc.resolve(n))
        && let Some(names) = names.as_array()
    {
        for pair in names.chunks(2) {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            if let Some(key) = doc.resolve(&pair[0]).as_string() {
                // The file's own name when it gives one, else the key.
                let shown = pair.get(1).and_then(|spec| doc.dict(spec)).and_then(|spec| text(doc, &spec, b"UF").or_else(|| text(doc, &spec, b"F")));
                out.push(shown.unwrap_or_else(|| key.to_text()));
            }
        }
    }
    if let Some(kids) = node.get(b"Kids").map(|k| doc.resolve(k))
        && let Some(kids) = kids.as_array()
    {
        for kid in kids {
            if *budget == 0 {
                return;
            }
            name_tree(doc, kid, out, budget);
        }
    }
}

/// How many fields a form has (its terminal fields).
fn fields(doc: &Cos, root: &Dict) -> u32 {
    let Some(form) = root.get(b"AcroForm").and_then(|f| doc.dict(f)) else { return 0 };
    let mut stack: Vec<Object> = form.get(b"Fields").map(|f| doc.resolve(f)).and_then(|f| f.as_array().cloned()).unwrap_or_default();
    let (mut count, mut seen) = (0u32, 0usize);
    while let Some(field) = stack.pop() {
        seen += 1;
        if seen > 50_000 {
            break;
        }
        let Some(dict) = doc.dict(&field) else { continue };
        let kids: Vec<Object> = dict.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default();
        // A field whose kids are only widgets (no names of their own) is one.
        let named_kids: Vec<Object> = kids.into_iter().filter(|kid| doc.dict(kid).is_some_and(|k| k.get(b"T").is_some())).collect();
        if named_kids.is_empty() {
            count += 1;
        } else {
            stack.extend(named_kids);
        }
    }
    count
}

/// The document's properties.
pub(crate) fn read(doc: &Cos) -> DocumentProperties {
    let mut properties = DocumentProperties::default();
    let root = doc.root().and_then(|r| doc.dict(&Object::Ref(r)));
    // The header's version, or the catalog's when it is later.
    let header = doc.version().trim().to_string();
    let catalog = root.as_ref().and_then(|root| root.name(b"Version")).map(|v| String::from_utf8_lossy(v).into_owned());
    let later = |a: &str, b: &str| {
        let parse = |v: &str| v.split_once('.').and_then(|(x, y)| Some((x.parse::<u32>().ok()?, y.parse::<u32>().ok()?)));
        if parse(b) > parse(a) { b.to_string() } else { a.to_string() }
    };
    properties.version = match catalog {
        Some(catalog) if !header.is_empty() => Some(later(&header, &catalog)),
        Some(catalog) => Some(catalog),
        None => (!header.is_empty()).then_some(header),
    };
    if let Some(info) = doc.trailer().get(b"Info").and_then(|i| doc.dict(i)) {
        properties.title = text(doc, &info, b"Title");
        properties.author = text(doc, &info, b"Author");
        properties.subject = text(doc, &info, b"Subject");
        properties.keywords = text(doc, &info, b"Keywords");
        properties.creator = text(doc, &info, b"Creator").or_else(|| text(doc, &info, b"Producer"));
        properties.created = text(doc, &info, b"CreationDate").and_then(|d| date(&d));
    }
    let mut fonts = Fonts { doc, found: BTreeMap::new(), seen: HashSet::new() };
    for page in pdfcraft_model::pages(doc) {
        if let Some(resources) = page.dict.get(b"Resources") {
            fonts.resources(resources, 0);
        }
    }
    properties.fonts = fonts
        .found
        .into_iter()
        .map(|(name, (embedded, standard))| FontInfo { name, embedded, standard: standard && !embedded })
        .collect();
    if let Some(root) = &root {
        if let Some(files) = root.get(b"Names").and_then(|n| doc.dict(n)).and_then(|names| names.get(b"EmbeddedFiles").cloned()) {
            let mut budget = 10_000;
            name_tree(doc, &files, &mut properties.attachments, &mut budget);
        }
        properties.fields = fields(doc, root);
        if let Some(ocgs) = root
            .get(b"OCProperties")
            .and_then(|o| doc.dict(o))
            .and_then(|o| o.get(b"OCGs").map(|g| doc.resolve(g)))
            .and_then(|g| g.as_array().cloned())
        {
            properties.layers = ocgs.iter().filter_map(|g| doc.dict(g)).map(|g| text(doc, &g, b"Name").unwrap_or_default()).collect();
        }
    }
    properties.permissions = doc.permissions().map(|p| Permissions { encrypted: true, print: p.print(), copy: p.copy(), modify: p.modify(), annotate: p.annotate() });
    properties
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_cos::PdfString;

    #[path = "../../../tests/fixtures/mod.rs"]
    mod fixtures;

    fn font(cos: &mut Cos, base: &str, file: Option<&str>) -> Object {
        let mut font = Dict::new();
        font.set("Type", Object::name("Font"));
        font.set("Subtype", Object::name("TrueType"));
        font.set("BaseFont", Object::name(base));
        if let Some(key) = file {
            let mut descriptor = Dict::new();
            descriptor.set("Type", Object::name("FontDescriptor"));
            descriptor.set(key, Object::Int(0));
            font.set("FontDescriptor", Object::Dict(descriptor));
        }
        Object::Ref(cos.add(Object::Dict(font)))
    }

    /// The rich fixture with every kind of font, a form inside the page, an
    /// attachment, two fields (one with two widgets), a layer, the Info
    /// fields and a later catalog version.
    fn document() -> Cos {
        let mut cos = Cos::open(std::sync::Arc::new(fixtures::build_rich())).unwrap();
        // Page 0's fonts: one embedded subset (used twice, once on a form),
        // one missing, a standard one, and a Type3.
        let subset = font(&mut cos, "ABCDEF+Garamond", Some("FontFile2"));
        let subset_again = font(&mut cos, "QWERTY+Garamond", Some("FontFile2"));
        let missing = font(&mut cos, "Calibri", None);
        let standard = font(&mut cos, "Helvetica", None);
        let mut type3 = Dict::new();
        type3.set("Type", Object::name("Font"));
        type3.set("Subtype", Object::name("Type3"));
        type3.set("BaseFont", Object::name("Glyphs"));
        let type3 = Object::Ref(cos.add(Object::Dict(type3)));
        let mut form_fonts = Dict::new();
        form_fonts.set("F9", subset_again);
        form_fonts.set("F8", font(&mut cos, "Wingdings", None));
        let mut form_resources = Dict::new();
        form_resources.set("Font", Object::Dict(form_fonts));
        let mut form = Dict::new();
        form.set("Subtype", Object::name("Form"));
        form.set("Resources", Object::Dict(form_resources));
        let form = Object::Ref(cos.add(Object::Dict(form)));
        let page = pdfcraft_model::pages(&cos)[0].obj;
        cos.update_dict(page, |page| {
            let mut fonts = Dict::new();
            fonts.set("F1", subset);
            fonts.set("F2", missing);
            fonts.set("F3", standard);
            fonts.set("F4", type3);
            let mut xobjects = Dict::new();
            xobjects.set("X1", form);
            let mut resources = Dict::new();
            resources.set("Font", Object::Dict(fonts));
            resources.set("XObject", Object::Dict(xobjects));
            page.set("Resources", Object::Dict(resources));
        })
        .unwrap();
        // An attachment, two fields, a layer.
        let mut spec = Dict::new();
        spec.set("Type", Object::name("Filespec"));
        spec.set("UF", Object::String(PdfString::text("data.csv")));
        let mut files = Dict::new();
        files.set("Names", Object::Array(vec![Object::String(PdfString::text("key")), Object::Dict(spec)]));
        let mut names = Dict::new();
        names.set("EmbeddedFiles", Object::Dict(files));
        let widget = |cos: &mut Cos| {
            let mut w = Dict::new();
            w.set("Subtype", Object::name("Widget"));
            Object::Ref(cos.add(Object::Dict(w)))
        };
        let (w1, w2) = (widget(&mut cos), widget(&mut cos));
        let mut name_field = Dict::new();
        name_field.set("FT", Object::name("Tx"));
        name_field.set("T", Object::String(PdfString::text("name")));
        let mut choice = Dict::new();
        choice.set("FT", Object::name("Btn"));
        choice.set("T", Object::String(PdfString::text("choice")));
        choice.set("Kids", Object::Array(vec![w1, w2]));
        let fields = vec![Object::Ref(cos.add(Object::Dict(name_field))), Object::Ref(cos.add(Object::Dict(choice)))];
        let mut acro = Dict::new();
        acro.set("Fields", Object::Array(fields));
        let mut layer = Dict::new();
        layer.set("Type", Object::name("OCG"));
        layer.set("Name", Object::String(PdfString::text("Notes")));
        let layer = Object::Ref(cos.add(Object::Dict(layer)));
        let mut oc = Dict::new();
        oc.set("OCGs", Object::Array(vec![layer]));
        let root = cos.root().unwrap();
        cos.update_dict(root, |catalog| {
            catalog.set("Names", Object::Dict(names));
            catalog.set("AcroForm", Object::Dict(acro));
            catalog.set("OCProperties", Object::Dict(oc));
            catalog.set("Version", Object::name("2.0"));
        })
        .unwrap();
        let mut info = Dict::new();
        info.set("Title", Object::String(PdfString::text("论文 Notes")));
        info.set("Subject", Object::String(PdfString::text("Attention")));
        info.set("Keywords", Object::String(PdfString::text("transformer, attention")));
        info.set("Producer", Object::String(PdfString::text("Typesetter 3")));
        info.set("CreationDate", Object::String(PdfString::text("D:20240301120000Z")));
        let info = cos.add(Object::Dict(info));
        cos.trailer_mut().set("Info", Object::Ref(info));
        cos
    }

    /// Every fact the dialog shows, from one document.
    #[test]
    fn a_pdfs_properties() {
        let properties = read(&document());
        let fonts: Vec<(&str, bool, bool)> = properties.fonts.iter().map(|f| (f.name.as_str(), f.embedded, f.standard)).collect();
        assert_eq!(
            fonts,
            [("Calibri", false, false), ("Garamond", true, false), ("Glyphs", true, false), ("Helvetica", false, true), ("Wingdings", false, false)],
            "one Garamond (two subsets, one inside a form); Type3 counts as embedded; Helvetica is standard; the form's own font is found"
        );
        assert_eq!(properties.attachments, ["data.csv"]);
        assert_eq!(properties.fields, 2, "a field with two widgets is one field");
        assert_eq!(properties.layers, ["Notes"]);
        assert_eq!(properties.version.as_deref(), Some("2.0"), "the catalog's later version");
        assert_eq!(properties.title.as_deref(), Some("论文 Notes"));
        assert_eq!(properties.subject.as_deref(), Some("Attention"));
        assert_eq!(properties.keywords.as_deref(), Some("transformer, attention"));
        assert_eq!(properties.creator.as_deref(), Some("Typesetter 3"), "no Creator: the Producer");
        assert_eq!(properties.created.as_deref(), Some("2024-03-01"));
        assert_eq!(properties.permissions, None, "no security settings");
    }

    /// A plain document says nothing it doesn't have, and an encrypted one
    /// says what its settings allow.
    #[test]
    fn absent_facts_stay_absent_and_permissions_are_read() {
        let plain = read(&Cos::open(std::sync::Arc::new(fixtures::build())).unwrap());
        assert!(plain.attachments.is_empty() && plain.layers.is_empty() && plain.fields == 0);
        let mut cos = Cos::open(std::sync::Arc::new(fixtures::build_rich())).unwrap();
        cos.set_encryption(&pdfcraft_cos::NewEncryption {
            algorithm: pdfcraft_cos::Algorithm::Aes256,
            user_password: "",
            owner_password: "owner",
            // Print allowed (bit 3), nothing else.
            permissions: -4 & !(16 | 32 | 8) ,
            encrypt_metadata: true,
            seed: [5; 32],
        })
        .unwrap();
        let bytes = pdfcraft_cos::write_full(&cos, &pdfcraft_cos::SaveOptions::default()).unwrap();
        let reopened = Cos::open(std::sync::Arc::new(bytes)).unwrap();
        let permissions = read(&reopened).permissions.expect("security settings");
        assert!(permissions.encrypted && permissions.print);
        assert!(!permissions.copy && !permissions.modify && !permissions.annotate, "{permissions:?}");
    }

    /// What a properties request costs on a real document: the 55-page
    /// manual. `MIREADER_TEST_PDF=<a.pdf> cargo test --release properties_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a sample PDF"]
    fn properties_cost() {
        let path = std::env::var("MIREADER_TEST_PDF").expect("MIREADER_TEST_PDF");
        let cos = Cos::open(std::sync::Arc::new(std::fs::read(&path).unwrap())).unwrap();
        let mut times = Vec::new();
        let mut last = DocumentProperties::default();
        for _ in 0..5 {
            let started = std::time::Instant::now();
            last = read(&cos);
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        eprintln!(
            "{} pages: properties in {:.1} ms (median of 5; min {:.1}); {} fonts, {} not embedded",
            pdfcraft_model::pages(&cos).len(),
            times[2],
            times[0],
            last.fonts.len(),
            last.fonts.iter().filter(|f| !f.embedded && !f.standard).count()
        );
    }

    #[test]
    fn subset_prefixes_and_dates() {
        assert_eq!(without_subset("ABCDEF+Arial-BoldMT"), "Arial-BoldMT");
        assert_eq!(without_subset("Abcdef+Arial"), "Abcdef+Arial", "only six capitals are a subset tag");
        assert_eq!(without_subset("Helvetica"), "Helvetica");
        assert_eq!(date("D:20120715103000+02'00'").as_deref(), Some("2012-07-15"));
        assert_eq!(date("D:2012").as_deref(), None);
        assert_eq!(date("D:20121340").as_deref(), None, "month 13");
    }
}
