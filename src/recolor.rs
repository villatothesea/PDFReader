//! [`Paper::Theme`] for plain pages: every pixel's lightness is remapped so
//! white lands on the host's page ground and black on its text colour, hue
//! and chroma kept (OKLab). Raster images are left exactly as drawn.
//!
//! [`Paper::Theme`]: mireader_reader_sdk::Paper::Theme

use std::collections::HashMap;

use hayro::kurbo::Rect;

/// Remap `rgba` (premultiplied RGBA8, `width` × `height`) in place.
/// `images` are device-pixel rects whose pixels must not change.
pub fn remap(rgba: &mut [u8], width: u32, height: u32, background: [u8; 3], ink: [u8; 3], images: &[Rect]) {
    let ground = to_oklab(background);
    let text = to_oklab(ink);
    let mut memo: HashMap<[u8; 3], [u8; 3]> = HashMap::new();
    let (width, height) = (width as usize, height as usize);
    // Per row, the column spans covered by images.
    let spans: Vec<(usize, usize, usize, usize)> = images
        .iter()
        .map(|rect| {
            let clamp = |v: f64, max: usize| (v.max(0.0) as usize).min(max);
            (
                clamp(rect.x0.floor(), width),
                clamp(rect.x1.ceil(), width),
                clamp(rect.y0.floor(), height),
                clamp(rect.y1.ceil(), height),
            )
        })
        .filter(|(x0, x1, y0, y1)| x1 > x0 && y1 > y0)
        .collect();
    for y in 0..height {
        let row_spans: Vec<(usize, usize)> = spans
            .iter()
            .filter(|(_, _, y0, y1)| (*y0..*y1).contains(&y))
            .map(|(x0, x1, _, _)| (*x0, *x1))
            .collect();
        for x in 0..width {
            if row_spans.iter().any(|(x0, x1)| (*x0..*x1).contains(&x)) {
                continue;
            }
            let at = (y * width + x) * 4;
            let alpha = rgba[at + 3];
            if alpha == 0 {
                continue;
            }
            // Premultiplied in, premultiplied out.
            let straight = |c: u8| ((c as u32 * 255 + alpha as u32 / 2) / alpha as u32).min(255) as u8;
            let rgb = [straight(rgba[at]), straight(rgba[at + 1]), straight(rgba[at + 2])];
            let mapped = *memo.entry(rgb).or_insert_with(|| remap_color(rgb, ground, text));
            for (channel, value) in mapped.iter().enumerate() {
                rgba[at + channel] = ((*value as u32 * alpha as u32 + 127) / 255) as u8;
            }
        }
    }
}

/// White → `ground`, black → `text`; the lightness in between maps
/// linearly, chroma follows the source scaled to the new range.
pub fn remap_color(rgb: [u8; 3], ground: [f32; 3], text: [f32; 3]) -> [u8; 3] {
    let [l, a, b] = to_oklab(rgb);
    let t = l.clamp(0.0, 1.0);
    let lightness = text[0] + (ground[0] - text[0]) * t;
    // Neutral source pixels take the theme's own tint (a dark theme's blue
    // ground stays blue); coloured ones keep their hue.
    let tint = |i: usize| text[i] + (ground[i] - text[i]) * t;
    from_oklab([lightness, a + tint(1), b + tint(2)])
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

pub fn to_oklab(rgb: [u8; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(srgb_to_linear);
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

pub fn from_oklab(lab: [f32; 3]) -> [u8; 3] {
    let [l, a, b] = lab;
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ]
    .map(linear_to_srgb)
}
