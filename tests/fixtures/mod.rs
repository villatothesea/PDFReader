//! PDFs written in code, with correct cross-reference tables.

#![allow(dead_code)]

/// A two-page PDF. Page 1 is 200×100 pt with a blue rectangle at
/// (20,20)–(180,80); page 2 is 300×150 pt and blank. The info dictionary
/// has a title (UTF-16BE) and an author.
pub fn build() -> Vec<u8> {
    let content = b"0 0 1 rg 20 20 160 60 re f\n";
    let title_utf16: String = std::iter::once("FEFF".to_string())
        .chain("Fixture Title".encode_utf16().map(|u| format!("{u:04X}")))
        .collect();
    write_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>".to_vec(),
            stream("", content),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 150] /Resources << >> >>".to_vec(),
            format!("<< /Title <{title_utf16}> /Author (Pat Printer) >>").into_bytes(),
        ],
        "/Info 6 0 R",
    )
}

/// Four 300×200 pt pages and an outline:
///
/// 1. text in Helvetica 20 pt: "Hello World" on the baseline y=150 (PDF
///    space; 50 pt from the top), "Second line" 30 pt below it;
/// 2. a red full-page fill, then black text "On red" — a declared ground;
/// 3. a 2×2 RGB image stretched over the whole page — an image page;
/// 4. blank.
///
/// Outline: "Intro" → page 1 (explicit `/Dest`) with child "Details" →
/// page 3; "Chapter 2" → page 2 through a `/GoTo` to the named
/// destination `(chap2)` in the catalog's `/Dests`.
pub fn build_rich() -> Vec<u8> {
    let font = "/Resources << /Font << /F1 8 0 R >> >>";
    let page = |contents: u32, resources: &str| {
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents {contents} 0 R {resources} >>")
            .into_bytes()
    };
    let image_pixels: &[u8] = &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
    write_pdf(
        &[
            // 1
            b"<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R /Dests << /chap2 [5 0 R /Fit] >> >>".to_vec(),
            // 2
            b"<< /Type /Pages /Kids [3 0 R 5 0 R 7 0 R 12 0 R] /Count 4 >>".to_vec(),
            // 3
            page(4, font),
            // 4
            stream(
                "",
                b"BT /F1 20 Tf 20 150 Td (Hello World) Tj 0 -30 Td (Second line) Tj ET\n",
            ),
            // 5
            page(6, font),
            // 6
            stream(
                "",
                b"1 0 0 rg 0 0 300 200 re f 0 0 0 rg BT /F1 20 Tf 20 150 Td (On red) Tj ET\n",
            ),
            // 7
            page(13, "/Resources << /XObject << /Im1 14 0 R >> >>"),
            // 8
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            // 9
            b"<< /Type /Outlines /First 10 0 R /Last 11 0 R /Count 2 >>".to_vec(),
            // 10
            b"<< /Title (Intro) /Parent 9 0 R /Next 11 0 R /Dest [3 0 R /XYZ 0 200 0] /First 15 0 R /Last 15 0 R /Count 1 >>".to_vec(),
            // 11
            b"<< /Title (Chapter 2) /Parent 9 0 R /Prev 10 0 R /A << /S /GoTo /D (chap2) >> >>".to_vec(),
            // 12
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> >>".to_vec(),
            // 13
            stream("", b"q 300 0 0 200 0 0 cm /Im1 Do Q\n"),
            // 14
            stream(
                "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
                image_pixels,
            ),
            // 15
            b"<< /Title (Details) /Parent 10 0 R /Dest [7 0 R /Fit] >>".to_vec(),
        ],
        "",
    )
}

/// Round 14 (#5): three 300×200 pt pages with links on page 1:
///
/// - `[20 140 120 170]` → page 3 by an explicit `/XYZ` destination, top at
///   y=120 (80 pt from the page's top);
/// - `[20 100 120 120]` → a `/GoTo` to the named destination `(chap1)`:
///   page 1, `/FitH` top 150 (50 pt from its top);
/// - `[20 60 120 80]` → a `/URI` action, `https://example.org/x`;
/// - `[150 60 250 80]` → a `/Launch` action (not followed: no link);
/// - a `/Text` note (not a link).
///
/// Page 2 is turned 90° (`/Rotate 90`) and links `[10 10 60 30]` to page 1.
pub fn build_linked() -> Vec<u8> {
    write_pdf(
        &[
            // 1
            b"<< /Type /Catalog /Pages 2 0 R /Dests << /chap1 [3 0 R /FitH 150] >> >>".to_vec(),
            // 2
            b"<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".to_vec(),
            // 3
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> /Annots [6 0 R 7 0 R 8 0 R 9 0 R 10 0 R] >>"
                .to_vec(),
            // 4
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Rotate 90 /Resources << >> /Annots [11 0 R] >>".to_vec(),
            // 5
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> >>".to_vec(),
            // 6
            b"<< /Type /Annot /Subtype /Link /Rect [20 140 120 170] /Dest [5 0 R /XYZ 0 120 0] >>".to_vec(),
            // 7
            b"<< /Type /Annot /Subtype /Link /Rect [20 100 120 120] /A << /S /GoTo /D (chap1) >> >>".to_vec(),
            // 8
            b"<< /Type /Annot /Subtype /Link /Rect [20 60 120 80] /A << /S /URI /URI (https://example.org/x) >> >>".to_vec(),
            // 9
            b"<< /Type /Annot /Subtype /Link /Rect [150 60 250 80] /A << /S /Launch /F (calc.exe) >> >>".to_vec(),
            // 10
            b"<< /Type /Annot /Subtype /Text /Rect [200 150 220 170] /Contents (a note) >>".to_vec(),
            // 11
            b"<< /Type /Annot /Subtype /Link /Rect [10 10 60 30] /Dest [3 0 R /Fit] >>".to_vec(),
        ],
        "",
    )
}

/// Round 19 (the corpus sweep, noembed-sjis): Japanese text — "日本",
/// <93FA967B> in Shift-JIS — in a Type0 font that isn't embedded (Adobe-Japan1,
/// 90ms-RKSJ-H, no font file): the reader has to bring a font for it.
pub fn build_noembed_cjk() -> Vec<u8> {
    write_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
            stream("", b"BT /F1 40 Tf 20 30 Td <93FA967B> Tj ET\n"),
            b"<< /Type /Font /Subtype /Type0 /BaseFont /MS-Gothic /Encoding /90ms-RKSJ-H /DescendantFonts [6 0 R] >>".to_vec(),
            b"<< /Type /Font /Subtype /CIDFontType0 /BaseFont /MS-Gothic /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 2 >> /FontDescriptor 7 0 R /DW 1000 >>".to_vec(),
            b"<< /Type /FontDescriptor /FontName /MS-Gothic /Flags 4 /FontBBox [0 -141 1000 859] /ItalicAngle 0 /Ascent 859 /Descent -141 /CapHeight 769 /StemV 80 >>".to_vec(),
        ],
        "",
    )
}

/// Round 19 (the corpus sweep, issue1350): two pages that draw the same
/// content — "Top half" at y 150, "Bottom half" at y 50 — each showing a
/// different region of it through its CropBox (top, then bottom).
pub fn build_cropped() -> Vec<u8> {
    let page = |crop: &str| {
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /CropBox [{crop}] /Contents 5 0 R /Resources << /Font << /F1 6 0 R >> >> >>")
            .into_bytes()
    };
    write_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
            page("0 100 300 200"),
            page("0 0 300 100"),
            stream("", b"BT /F1 20 Tf 20 150 Td (Top half) Tj 0 -100 Td (Bottom half) Tj ET\n"),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ],
        "",
    )
}

/// Round 19 (the corpus sweep, bug1650302): "Hi", <00480069>, in a Type0
/// font whose ToUnicode is the name /Identity-H rather than a CMap stream —
/// each two-byte code is its own Unicode value, as pdf.js and mupdf read it.
pub fn build_identity_to_unicode() -> Vec<u8> {
    write_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
            stream("", b"BT /F1 40 Tf 20 30 Td <00480069> Tj ET\n"),
            b"<< /Type /Font /Subtype /Type0 /BaseFont /Arial /Encoding /Identity-H /ToUnicode /Identity-H /DescendantFonts [6 0 R] >>".to_vec(),
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Arial /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R /DW 600 >>".to_vec(),
            b"<< /Type /FontDescriptor /FontName /Arial /Flags 32 /FontBBox [-665 -325 2000 1006] /ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 >>".to_vec(),
        ],
        "",
    )
}

/// Round 19 (the corpus sweep, text-layer group 5): `glyphs` (glyph ids of
/// `font`, a TrueType file) drawn through a Type0 font that embeds it, with
/// an Identity ordering and no ToUnicode — only the font's own cmap says
/// what they are.
pub fn build_embedded_identity(font: &[u8], glyphs: &[u32]) -> Vec<u8> {
    build_embedded_cid(font, "Identity", glyphs, "/Identity")
}

/// Round 19 (the corpus sweep, vertical, issue13147 and three more pages):
/// `cids` in an embedded TrueType CID font of the Adobe-`ordering`
/// collection, with no ToUnicode. Every CID draws glyph 0 (a CIDToGIDMap of
/// zeros), so the font's own cmap can't name them either: only the
/// collection's CID → Unicode table says what they are.
pub fn build_embedded_collection(font: &[u8], ordering: &str, cids: &[u32]) -> Vec<u8> {
    build_embedded_cid(font, ordering, cids, "9 0 R")
}

fn build_embedded_cid(font: &[u8], ordering: &str, cids: &[u32], cid_to_gid: &str) -> Vec<u8> {
    let codes: String = cids.iter().map(|g| format!("{g:04X}")).collect();
    let zeros = vec![0_u8; 2 * (cids.iter().copied().max().unwrap_or(0) as usize + 1)];
    write_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
            stream("", format!("BT /F1 40 Tf 20 30 Td <{codes}> Tj ET\n").as_bytes()),
            b"<< /Type /Font /Subtype /Type0 /BaseFont /ABCDEF+Arial /Encoding /Identity-H /DescendantFonts [6 0 R] >>".to_vec(),
            format!("<< /Type /Font /Subtype /CIDFontType2 /BaseFont /ABCDEF+Arial /CIDSystemInfo << /Registry (Adobe) /Ordering ({ordering}) /Supplement 0 >> /CIDToGIDMap {cid_to_gid} /FontDescriptor 7 0 R /DW 600 >>").into_bytes(),
            b"<< /Type /FontDescriptor /FontName /ABCDEF+Arial /Flags 32 /FontBBox [-665 -325 2000 1006] /ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 /FontFile2 8 0 R >>".to_vec(),
            stream(&format!("/Length1 {}", font.len()), font),
            stream("", &zeros),
        ],
        "",
    )
}

/// Round 19 (the corpus sweep, text-layer group 2, issue918 and nine more
/// pages): "Hi" in a Type 3 font with no ToUnicode. Its Encoding names the
/// glyphs /H and /i; only those names say what they are. The glyphs are
/// 500 units wide, the advance the scan assumes for Type 3 (it reads no
/// widths yet), so the test is about the text, not its spacing.
pub fn build_type3_named() -> Vec<u8> {
    build_type3(b"<0102> Tj", "[1 /H /i]", "/H 6 0 R /i 6 0 R", 1, 2, 500)
}

/// Round 19 (the corpus sweep, french_diacritics, issue918, issue9655):
/// "Hi" in a Type 3 font whose glyph names only number their own codes —
/// /a72 (decimal) and /C69 (hexadecimal) for codes 72 and 0x69.
pub fn build_type3_numbered() -> Vec<u8> {
    build_type3(b"<4869> Tj", "[72 /a72 105 /C69]", "/a72 6 0 R /C69 6 0 R", 72, 105, 500)
}

/// Round 19 (the corpus sweep, issue5896 "P a r a l l e l"): "Hi" twice in
/// a Type 3 font whose glyphs are 750 units wide — set tight, then with a
/// word gap pushed in by TJ.
pub fn build_type3_wide() -> Vec<u8> {
    build_type3(b"<0102> Tj 0 -30 Td [<01> -600 <02>] TJ", "[1 /H /i]", "/H 6 0 R /i 6 0 R", 1, 2, 750)
}

fn build_type3(show: &[u8], differences: &str, procs: &str, first: u32, last: u32, width: u32) -> Vec<u8> {
    let widths = vec![width.to_string(); (last - first + 1) as usize].join(" ");
    write_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
            stream("", &[b"BT /F1 40 Tf 20 30 Td ".as_slice(), show, b" ET\n"].concat()),
            format!("<< /Type /Font /Subtype /Type3 /FontBBox [0 0 500 700] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << {procs} >> /Encoding << /Type /Encoding /Differences {differences} >> /FirstChar {first} /LastChar {last} /Widths [{widths}] >>").into_bytes(),
            stream("", format!("{width} 0 0 0 {} 700 d1 50 0 {} 700 re f\n", width - 50, width - 100).as_bytes()),
        ],
        "",
    )
}

/// Round 19 (the corpus sweep, text-layer group 4, issue7492): "Hi" in a
/// Type0 font whose ToUnicode CMap is sound except for its header — a
/// `/CMapName` with spaces and commas in it, as a font path became one.
pub fn build_broken_cmap_header() -> Vec<u8> {
    let cmap = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
/CMapName /-usr-share-fonts-Panton Family-Fontfabric - Panton.otf,000-UTF16 def\n\
/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
2 beginbfchar\n<0001> <0048>\n<0002> <0069>\nendbfchar\nendcmap\n\
CMapName currentdict /CMap defineresource pop\nend\nend\n";
    write_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
            stream("", b"BT /F1 40 Tf 20 30 Td <00010002> Tj ET\n"),
            b"<< /Type /Font /Subtype /Type0 /BaseFont /Arial /Encoding /Identity-H /ToUnicode 8 0 R /DescendantFonts [6 0 R] >>".to_vec(),
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Arial /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R /DW 600 >>".to_vec(),
            b"<< /Type /FontDescriptor /FontName /Arial /Flags 32 /FontBBox [-665 -325 2000 1006] /ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 >>".to_vec(),
            stream("", cmap),
        ],
        "",
    )
}

/// Round 19 (the corpus sweep, P2): a page whose file draws nothing of its
/// form or comment — a text field holding "Hello" (rect 20 60 180 90), a
/// checked check box (rect 20 20 40 40) and a red square comment (rect
/// 120 10 180 50), none with an appearance (`/AP`). Drawing them is left to
/// the viewer.
pub fn build_without_appearances() -> Vec<u8> {
    write_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm 8 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> /Annots [5 0 R 6 0 R 7 0 R] >>".to_vec(),
            stream("", b""),
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Hello) /DA (/Helv 12 Tf 0 g) /Rect [20 60 180 90] /P 3 0 R /F 4 >>".to_vec(),
            b"<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /V /Yes /Rect [20 20 40 40] /P 3 0 R /F 4 /MK << /BC [0 0 0] >> >>".to_vec(),
            b"<< /Type /Annot /Subtype /Square /Rect [120 10 180 50] /C [1 0 0] /BS << /W 3 >> /P 3 0 R /F 4 >>".to_vec(),
            b"<< /Fields [5 0 R 6 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 9 0 R >> >> >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
        ],
        "",
    )
}

/// Round 19 (the corpus sweep, text_field_own_canvas_calc): a form that
/// sets /NeedAppearances, with a text field that has no value (/V) but an
/// appearance of its own — a black box over rect 20 20 180 80, as a
/// calculated field is drawn.
pub fn build_need_appearances_own_look() -> Vec<u8> {
    write_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> /Annots [5 0 R] >>".to_vec(),
            stream("", b""),
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (total) /DA (/Helv 12 Tf 0 g) /Rect [20 20 180 80] /P 3 0 R /F 4 /AP << /N 8 0 R >> >>".to_vec(),
            b"<< /Fields [5 0 R] /NeedAppearances true /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 7 0 R >> >> >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
            stream("/Type /XObject /Subtype /Form /BBox [0 0 160 60]", b"0 g 0 0 160 60 re f\n"),
        ],
        "",
    )
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    [
        format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes(),
        data.to_vec(),
        b"\nendstream".to_vec(),
    ]
    .concat()
}

/// Objects numbered from 1, a classic xref table and a trailer.
fn write_pdf(objects: &[Vec<u8>], trailer_extra: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (ix, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", ix + 1).into_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").into_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .into_bytes(),
    );
    out
}

/// Every pixel of a PNG as straight RGBA, with its width.
pub fn pixels(png: &[u8]) -> (Vec<[u8; 4]>, u32) {
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut buffer).unwrap();
    let channels = frame.color_type.samples();
    let pixels = buffer[..frame.buffer_size()]
        .chunks_exact(channels)
        .map(|px| match channels {
            4 => [px[0], px[1], px[2], px[3]],
            3 => [px[0], px[1], px[2], 255],
            2 => [px[0], px[0], px[0], px[1]],
            _ => [px[0], px[0], px[0], 255],
        })
        .collect();
    (pixels, frame.width)
}

/// RGBA of the pixel at (x, y) in a PNG.
pub fn pixel(png: &[u8], x: u32, y: u32) -> [u8; 4] {
    let (pixels, width) = pixels(png);
    pixels[(y * width + x) as usize]
}
