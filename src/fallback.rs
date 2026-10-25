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
use hayro::hayro_interpret::hayro_cmap::{CMapName, CidFamily};

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

/// Round 19 (the corpus sweep, bug1650302): the Identity CMaps, written so
/// they also serve as a ToUnicode. A font's /Encoding /Identity-H never
/// reaches the CMap resolver (hayro builds that one itself); a /ToUnicode
/// /Identity-H does, and parsed as the plain CID CMap it maps no code to
/// text. pdf.js and mupdf read it as "each code is its own Unicode value",
/// so these carry that as bfranges beside the identity CID range (kept for
/// a CMap that `usecmap`s one). Surrogate codes map to nothing.
fn identity_cmap(vertical: bool) -> &'static [u8] {
    static HORIZONTAL: OnceLock<Vec<u8>> = OnceLock::new();
    static VERTICAL: OnceLock<Vec<u8>> = OnceLock::new();
    let build = || {
        let name = if vertical { "Identity-V" } else { "Identity-H" };
        let mut out = format!(
            "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
             /CMapName /{name} def /CMapType 2 def /WMode {} def\n\
             1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
             1 begincidrange <0000> <FFFF> 0 endcidrange\n",
            u8::from(vertical)
        );
        let highs: Vec<u32> = (0x00..=0xFF).filter(|high| !(0xD8..=0xDF).contains(high)).collect();
        for block in highs.chunks(100) {
            out.push_str(&format!("{} beginbfrange\n", block.len()));
            for high in block {
                out.push_str(&format!("<{high:02X}00> <{high:02X}FF> <{high:02X}00>\n"));
            }
            out.push_str("endbfrange\n");
        }
        out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop end end\n");
        out.into_bytes()
    };
    if vertical { VERTICAL.get_or_init(build) } else { HORIZONTAL.get_or_init(build) }
}

/// hayro's settings with the CJK fallback and the Identity ToUnicode.
pub(crate) fn settings() -> InterpreterSettings {
    let default = InterpreterSettings::default();
    let standard = default.font_resolver.clone();
    let standard_cmaps = default.cmap_resolver.clone();
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
        cmap_resolver: Arc::new(move |name| match name {
            CMapName::IdentityH => Some(identity_cmap(false)),
            CMapName::IdentityV => Some(identity_cmap(true)),
            other => standard_cmaps(other),
        }),
        ..default
    }
}
