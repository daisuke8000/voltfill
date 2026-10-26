pub fn pdf_with_text(text: &str) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
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
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![80.into(), 240.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        content
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
