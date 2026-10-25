//! Round 19 (the corpus sweep, text-layer group 5): text for a glyph whose
//! font gives none. A Type0 font with no ToUnicode (and an Identity
//! ordering, so no Adobe character collection to look the CIDs up in)
//! leaves hayro with no text for its glyphs, so the page had no text layer.
//! The font's own cmap still says which character each glyph draws: read
//! backwards, glyph → character, as pdf.js does.
//!
//! Only for an embedded font. A stand-in's glyphs show the wrong letters
//! (a Latin font asked for CIDs it doesn't know), and text naming them
//! would make search find what the file never said.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use hayro::hayro_interpret::font::OutlineGlyph;
use skrifa::MetadataProvider as _;

/// Fonts read so far, by hayro's cache key: glyph → character, or `None`
/// when the font has no usable cmap. Cleared past this many fonts.
const FONTS_KEPT: usize = 64;
type Reverse = Option<Arc<HashMap<u32, char>>>;
static READ: OnceLock<Mutex<HashMap<u128, Reverse>>> = OnceLock::new();

/// The character `glyph` draws, from its embedded font's cmap.
pub(crate) fn from_font_cmap(glyph: &OutlineGlyph) -> Option<String> {
    let data = glyph.font_data()?;
    let bytes: &[u8] = (*data.data).as_ref();
    if crate::fallback::is_stand_in(bytes) {
        return None;
    }
    let reverse = {
        let read = READ.get_or_init(|| Mutex::new(HashMap::new()));
        let mut read = read.lock().ok()?;
        if read.len() >= FONTS_KEPT && !read.contains_key(&data.cache_key) {
            read.clear();
        }
        read.entry(data.cache_key).or_insert_with(|| reverse_cmap(bytes)).clone()?
    };
    reverse.get(&glyph.glyph_id().to_u32()).map(|c| c.to_string())
}

/// Glyph → character over the font's cmap. A glyph several characters map
/// to takes the lowest one that isn't private use (the private-use areas
/// name nothing a reader could search for).
fn reverse_cmap(bytes: &[u8]) -> Reverse {
    let font = skrifa::FontRef::new(bytes).ok()?;
    let mut reverse: HashMap<u32, char> = HashMap::new();
    for (code, glyph) in font.charmap().mappings() {
        let Some(c) = char::from_u32(code) else { continue };
        if is_private_use(c) || c.is_control() {
            continue;
        }
        reverse
            .entry(glyph.to_u32())
            .and_modify(|kept| *kept = (*kept).min(c))
            .or_insert(c);
    }
    (!reverse.is_empty()).then(|| Arc::new(reverse))
}

fn is_private_use(c: char) -> bool {
    matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}
