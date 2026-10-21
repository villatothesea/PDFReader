//! A two-page PDF written in code, with a correct cross-reference table.
//!
//! Page 1 is 200×100 pt with a blue rectangle at (20,20)–(180,80); page 2
//! is 300×150 pt and blank. The info dictionary has a title (UTF-16BE) and
//! an author.

pub fn build() -> Vec<u8> {
    let content = b"0 0 1 rg 20 20 160 60 re f\n";
    let title_utf16: String = std::iter::once("FEFF".to_string())
        .chain("Fixture Title".encode_utf16().map(|u| format!("{u:04X}")))
        .collect();
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>".to_vec(),
        [
            format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
            content.to_vec(),
            b"endstream".to_vec(),
        ]
        .concat(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 150] /Resources << >> >>".to_vec(),
        format!("<< /Title <{title_utf16}> /Author (Pat Printer) >>").into_bytes(),
    ];
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
            "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .into_bytes(),
    );
    out
}

/// RGBA of the pixel at (x, y) in a PNG.
pub fn pixel(png: &[u8], x: u32, y: u32) -> [u8; 4] {
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut buffer).unwrap();
    let channels = frame.color_type.samples();
    let at = (y as usize * frame.width as usize + x as usize) * channels;
    let px = &buffer[at..at + channels];
    match channels {
        4 => [px[0], px[1], px[2], px[3]],
        3 => [px[0], px[1], px[2], 255],
        _ => [px[0], px[0], px[0], 255],
    }
}
