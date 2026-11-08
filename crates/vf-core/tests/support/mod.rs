//! Synthetic PDFs drawn with a Type3 font (every glyph a filled square one em
//! wide), so renderers need no font file and the text layer is exact.
#![allow(dead_code, reason = "each test binary uses its own subset")]

/// One page with `text` at (80, 240).
pub fn pdf_with_text(text: &str) -> Vec<u8> {
    pdf_with_pages(&[&[(80.0, 240.0, text)]])
}

/// US Letter pages (612 x 792) with 12 pt runs at PDF coordinates (x, y,
/// text). A page without runs gets only a filled rectangle, like a scan
/// without a text layer.
pub fn pdf_with_pages(pages: &[&[(f64, f64, &str)]]) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let text: String = pages
        .iter()
        .flat_map(|runs| runs.iter().map(|(_, _, text)| *text))
        .collect();
    // Type3 glyphs need no font file, which renderers refuse to substitute.
    let glyph = |c: char| match c {
        ' ' => "space".to_owned(),
        '0'..='9' => [
            "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
        ][c as usize - '0' as usize]
            .to_owned(),
        _ => c.to_string(),
    };
    let mut procs = lopdf::Dictionary::new();
    let mut differences = Vec::new();
    for c in text.chars() {
        let name = glyph(c);
        if !procs.has(name.as_bytes()) {
            let proc = doc.add_object(Stream::new(
                dictionary! {},
                b"1000 0 0 0 750 750 d1 0 0 750 750 re f".to_vec(),
            ));
            procs.set(name.as_str(), proc);
            differences.push(Object::Integer(c as i64));
            differences.push(Object::Name(name.into_bytes()));
        }
    }
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <00> <FF> endcodespacerange\n",
    );
    let chars: std::collections::BTreeSet<char> = text.chars().collect();
    cmap += &format!("{} beginbfchar\n", chars.len());
    for c in chars {
        cmap += &format!("<{:02X}> <{:04X}>\n", c as u32, c as u32);
    }
    cmap += "endbfchar endcmap CMapName currentdict /CMap defineresource pop end end";
    let to_unicode = doc.add_object(Stream::new(dictionary! {}, cmap.into_bytes()));
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type3", "ToUnicode" => to_unicode,
        "FontBBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
        "FontMatrix" => vec![0.001.into(), 0.into(), 0.into(), 0.001.into(), 0.into(), 0.into()],
        "CharProcs" => procs,
        "Encoding" => dictionary! {"Type" => "Encoding", "Differences" => differences},
        "FirstChar" => 32, "LastChar" => 126,
        "Widths" => vec![Object::Integer(1000); 95],
    });
    let mut kids = Vec::new();
    for runs in pages {
        let mut operations = Vec::new();
        if runs.is_empty() {
            operations.push(Operation::new("g", vec![0.5.into()]));
            operations.push(Operation::new(
                "re",
                vec![72.into(), 500.into(), 300.into(), 200.into()],
            ));
            operations.push(Operation::new("f", vec![]));
        }
        for (x, y, text) in runs.iter() {
            operations.extend([
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![(*x).into(), (*y).into()]),
                Operation::new("Tj", vec![Object::string_literal(*text)]),
                Operation::new("ET", vec![]),
            ]);
        }
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations }
                .encode()
                .expect("synthetic page operations must encode"),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {"Font" => dictionary! {"F1" => font_id}},
        });
        kids.push(Object::from(page_id));
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => kids, "Count" => count,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .expect("synthetic PDF must serialize");
    bytes
}

/// One text show in [`pdf_with_runs`]: the text matrix's linear part, its
/// origin and the shown text.
pub type Run<'a> = ([f64; 4], (f64, f64), &'a str);

/// A one-page PDF drawing each run with its own text matrix in a composite
/// font whose codes are the characters' (BMP) code points; every glyph
/// advances one em (12 pt).
pub fn pdf_with_runs(runs: &[Run<'_>]) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, StringFormat, dictionary};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let chars: std::collections::BTreeSet<char> = runs.iter().flat_map(|r| r.2.chars()).collect();
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> endcodespacerange\n",
    );
    cmap += &format!("{} beginbfchar\n", chars.len());
    for c in chars {
        cmap += &format!("<{:04X}> <{:04X}>\n", c as u32, c as u32);
    }
    cmap += "endbfchar endcmap CMapName currentdict /CMap defineresource pop end end";
    let to_unicode = doc.add_object(Stream::new(dictionary! {}, cmap.into_bytes()));
    let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "TestCID", "Flags" => 4,
        "FontBBox" => vec![0.into(), (-200).into(), 1000.into(), 800.into()],
        "ItalicAngle" => 0, "Ascent" => 800, "Descent" => -200, "CapHeight" => 700, "StemV" => 80,
    });
    let descendant = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "TestCID",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor, "DW" => 1000, "CIDToGIDMap" => "Identity",
    });
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "TestCID",
        "Encoding" => "Identity-H", "DescendantFonts" => vec![descendant.into()],
        "ToUnicode" => to_unicode,
    });
    let mut operations = Vec::new();
    for &([a, b, c, d], (x, y), text) in runs {
        let bytes = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        operations.extend([
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new(
                "Tm",
                [a, b, c, d, x, y]
                    .iter()
                    .map(|&v| Object::Real(v as f32))
                    .collect(),
            ),
            Operation::new("Tj", vec![Object::String(bytes, StringFormat::Hexadecimal)]),
            Operation::new("ET", vec![]),
        ]);
    }
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations }
            .encode()
            .expect("synthetic text operations must encode"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! {"Font" => dictionary! {"F1" => font_id}},
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .expect("synthetic PDF must serialize");
    bytes
}
