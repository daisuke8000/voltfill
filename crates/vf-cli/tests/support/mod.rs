//! Synthetic PDFs shared by the CLI's unit and integration tests.
#![allow(dead_code, reason = "each test binary uses its own subset")]

/// A PDF of `objects`, numbered from 1; object 1 is the catalog.
pub fn pdf(objects: &[String]) -> Vec<u8> {
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf += &format!("{} 0 obj\n{object}\nendobj\n", i + 1);
    }
    let xref = pdf.len();
    pdf += &format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        pdf += &format!("{offset:010} 00000 n \n");
    }
    pdf += &format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    pdf.into_bytes()
}
fn stream(content: &str) -> String {
    format!(
        "<< /Length {} >>\nstream\n{content}endstream",
        content.len()
    )
}
const HELVETICA: &str =
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";

/// `pages` Helvetica pages with the given MediaBox, each drawing `content`.
pub fn pages_pdf(pages: usize, media: &str, content: &str) -> Vec<u8> {
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 5 + i)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ),
        HELVETICA.to_owned(),
        stream(content),
    ];
    objects.extend((0..pages).map(|_| format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox {media} /Resources << /Font << /F1 3 0 R >> >> /Contents 4 0 R >>"
    )));
    pdf(&objects)
}
/// One Letter page with one Helvetica text line per entry.
pub fn text_pdf(lines: &[&str]) -> Vec<u8> {
    let content: String = lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("BT /F1 12 Tf 72 {} Td ({line}) Tj ET\n", 700 - 40 * i))
        .collect();
    pages_pdf(1, "[0 0 612 792]", &content)
}
/// One Letter page with a gray rectangle and no text layer, like a scan.
pub fn scanned_pdf() -> Vec<u8> {
    pages_pdf(1, "[0 0 612 792]", "0.5 g 72 500 300 200 re f\n")
}
/// One Letter page with `AB12` drawn in a Type3 font (each glyph a filled
/// square), so a renderer needs no font file.
pub fn renderable_text_pdf() -> Vec<u8> {
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <00> <FF> endcodespacerange\n4 beginbfchar\n<41> <0041>\n<42> <0042>\n<31> <0031>\n<32> <0032>\nendbfchar endcmap CMapName currentdict /CMap defineresource pop end end\n";
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A 6 0 R /B 6 0 R /one 6 0 R /two 6 0 R >> /Encoding << /Type /Encoding /Differences [49 /one /two 65 /A /B] >> /FirstChar 32 /LastChar 126 /Widths [{}] /ToUnicode 7 0 R >>",
            vec!["1000"; 95].join(" ")
        ),
        stream("BT /F1 12 Tf 72 700 Td (AB12) Tj ET\n"),
        stream("1000 0 0 0 750 750 d1 0 0 750 750 re f\n"),
        stream(cmap),
    ])
}
