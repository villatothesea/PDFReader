//! Round 19 (the corpus sweep): a font for CJK text whose font isn't
//! embedded. hayro asks its font resolver for a stand-in, and the default one
//! offers a Latin standard font, which has no CJK glyphs — so the page drew
//! nothing while its text could be selected (noembed-sjis, noembed-eucjp).
//! For a Japanese, Chinese or Korean character collection this resolver
//! offers the system's own font for it (read from the Windows font folder,
//! nothing bundled); everything else goes to the default as before.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::font::{FontData, FontQuery};
use hayro::hayro_interpret::hayro_cmap::CidFamily;

/// A font file read once is kept; one that isn't there is remembered too.
static LOADED: OnceLock<Mutex<HashMap<&'static str, Option<FontData>>>> = OnceLock::new();

/// Larger than any system CJK font; a file past this isn't read.
const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;

/// The system fonts that cover a character collection, best first.
fn candidates(family: &CidFamily) -> &'static [&'static str] {
    match family {
        CidFamily::AdobeJapan1 => &["YuGothM.ttc", "msgothic.ttc", "meiryo.ttc"],
        CidFamily::AdobeGB1 => &["msyh.ttc", "simsun.ttc"],
        CidFamily::AdobeCNS1 => &["msjh.ttc", "mingliu.ttc"],
        CidFamily::AdobeKorea1 => &["malgun.ttf", "gulim.ttc", "batang.ttc"],
        _ => &[],
    }
}

fn fonts_dir() -> PathBuf {
    std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("Fonts")
}

/// The first of `names` the system has, as font data (face 0).
fn system_font(names: &[&'static str]) -> Option<(FontData, u32)> {
    let loaded = LOADED.get_or_init(|| Mutex::new(HashMap::new()));
    let mut loaded = loaded.lock().ok()?;
    for name in names {
        let entry = loaded.entry(name).or_insert_with(|| {
            let path = fonts_dir().join(name);
            let size = std::fs::metadata(&path).ok()?.len();
            if size > MAX_FONT_BYTES {
                return None;
            }
            std::fs::read(&path).ok().map(|bytes| Arc::new(bytes) as FontData)
        });
        if let Some(data) = entry {
            return Some((data.clone(), 0));
        }
    }
    None
}

/// Whether this system has a stand-in for `family` (tests skip without one).
#[cfg(test)]
pub(crate) fn has_font_for(family: &CidFamily) -> bool {
    system_font(candidates(family)).is_some()
}

/// hayro's settings with the CJK fallback.
pub(crate) fn settings() -> InterpreterSettings {
    let default = InterpreterSettings::default();
    let standard = default.font_resolver.clone();
    InterpreterSettings {
        font_resolver: Arc::new(move |query| {
            if let FontQuery::Fallback(fallback) = query
                && let Some(collection) = &fallback.character_collection
                && let Some(found) = system_font(candidates(&collection.family))
            {
                return Some(found);
            }
            standard(query)
        }),
        ..default
    }
}
