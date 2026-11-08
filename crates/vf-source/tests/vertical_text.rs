//! Vertical and rotated text must never reach layout as usable source text.
//! PDFs are built with lopdf; Type3 glyphs need no font program.
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};
use vf_source::layout::analyze;
use vf_source::{DocumentSnapshot, PdfLimits, Quality, SourceFragment, inspect};

/// One text show: font resource tag, text matrix and the shown string.
struct Run {
    font: &'static str,
    matrix: [f64; 6],
    text: Object,
}

/// An upright Type3 run at `(x, y)`.
fn upright(x: f64, y: f64, text: &str) -> Run {
    turned([1.0, 0.0, 0.0, 1.0], x, y, text)
}

/// A Type3 run with the text matrix's linear part `[a b c d]`.
fn turned(linear: [f64; 4], x: f64, y: f64, text: &str) -> Run {
    let [a, b, c, d] = linear;
    Run {
        font: "F1",
        matrix: [a, b, c, d, x, y],
        text: Object::string_literal(text),
    }
}

/// A run in a two-byte composite font whose codes are the ASCII codes.
fn composite(font: &'static str, x: f64, y: f64, text: &str) -> Run {
    let bytes = text.bytes().flat_map(|b| [0, b]).collect();
    Run {
        font,
        matrix: [1.0, 0.0, 0.0, 1.0, x, y],
        text: Object::String(bytes, StringFormat::Hexadecimal),
    }
}

fn operations(runs: &[Run]) -> Vec<Operation> {
    let mut operations = Vec::new();
    for run in runs {
        operations.push(Operation::new("BT", vec![]));
        operations.push(Operation::new(
            "Tf",
            vec![Object::Name(run.font.into()), 12.into()],
        ));
        operations.push(Operation::new(
            "Tm",
            run.matrix.iter().map(|&v| Object::Real(v as f32)).collect(),
        ));
        operations.push(Operation::new("Tj", vec![run.text.clone()]));
        operations.push(Operation::new("ET", vec![]));
    }
    operations
}

fn stream(doc: &mut Document, dict: Dictionary, operations: Vec<Operation>) -> ObjectId {
    let content = Content { operations }
        .encode()
        .expect("synthetic operations must encode");
    doc.add_object(Stream::new(dict, content))
}

fn glyph_name(c: char) -> String {
    match c {
        ' ' => "space".into(),
        ':' => "colon".into(),
        '-' => "hyphen".into(),
        '0'..='9' => [
            "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
        ][c as usize - '0' as usize]
            .into(),
        _ => c.to_string(),
    }
}

const CHARSET: &str = " :-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A Type3 font covering [`CHARSET`] with a ToUnicode map; every glyph is 1 em wide.
fn type3_font(doc: &mut Document) -> ObjectId {
    let mut procs = Dictionary::new();
    let mut differences = Vec::new();
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <00> <FF> endcodespacerange\n",
    );
    cmap += &format!("{} beginbfchar\n", CHARSET.len());
    for c in CHARSET.chars() {
        let proc = doc.add_object(Stream::new(
            dictionary! {},
            b"1000 0 0 0 750 750 d1 0 0 750 750 re f".to_vec(),
        ));
        procs.set(glyph_name(c), proc);
        differences.push(Object::Integer(c as i64));
        differences.push(Object::Name(glyph_name(c).into_bytes()));
        cmap += &format!("<{:02X}> <{:04X}>\n", c as u32, c as u32);
    }
    cmap += "endbfchar endcmap CMapName currentdict /CMap defineresource pop end end";
    let to_unicode = doc.add_object(Stream::new(dictionary! {}, cmap.into_bytes()));
    doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type3", "ToUnicode" => to_unicode,
        "FontBBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
        "FontMatrix" => vec![0.001.into(), 0.into(), 0.into(), 0.001.into(), 0.into(), 0.into()],
        "CharProcs" => procs,
        "Encoding" => dictionary! {"Type" => "Encoding", "Differences" => differences},
        "FirstChar" => 32, "LastChar" => 126,
        "Widths" => vec![Object::Integer(1000); 95],
    })
}

/// A Type0 font over a non-embedded CIDFontType2 with the given `/Encoding`
/// (a CMap name or an embedded CMap stream); codes `<00XX>` map to U+00XX.
fn type0_font(doc: &mut Document, encoding: Object) -> ObjectId {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> endcodespacerange\n",
    );
    cmap += &format!("{} beginbfchar\n", CHARSET.len());
    for c in CHARSET.chars() {
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
    doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "TestCID",
        "Encoding" => encoding, "DescendantFonts" => vec![descendant.into()],
        "ToUnicode" => to_unicode,
    })
}

/// An embedded Identity-style CMap stream.
fn cmap_stream(doc: &mut Document, dict: Dictionary, wmode: Option<u8>) -> ObjectId {
    let mut body = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Test-CMap def /CMapType 1 def\n",
    );
    if let Some(wmode) = wmode {
        body += &format!("/WMode {wmode} def\n");
    }
    body += "1 begincodespacerange <0000> <FFFF> endcodespacerange 1 begincidrange <0000> <FFFF> 0 endcidrange endcmap CMapName currentdict /CMap defineresource pop end end";
    let mut dict = dict;
    dict.set("Type", "CMap");
    dict.set("CMapName", "Test-CMap");
    doc.add_object(Stream::new(dict, body.into_bytes()))
}

struct Page {
    media: [i64; 4],
    rotate: Option<i64>,
    resources: Dictionary,
    operations: Vec<Operation>,
}

impl Page {
    fn new(fonts: Dictionary, runs: &[Run]) -> Self {
        Self {
            media: [0, 0, 612, 792],
            rotate: None,
            resources: dictionary! {"Font" => fonts},
            operations: operations(runs),
        }
    }
}

/// Build a document; `pages` adds its own objects (fonts, XObjects) first.
fn document(pages: impl FnOnce(&mut Document) -> Vec<Page>) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let specs = pages(&mut doc);
    let mut kids = Vec::new();
    for spec in specs {
        let content = stream(&mut doc, dictionary! {}, spec.operations);
        let mut page = dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content,
            "MediaBox" => spec.media.iter().map(|&v| Object::Integer(v)).collect::<Vec<_>>(),
            "Resources" => spec.resources,
        };
        if let Some(rotate) = spec.rotate {
            page.set("Rotate", rotate);
        }
        kids.push(Object::Reference(doc.add_object(page)));
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {"Type" => "Pages", "Kids" => kids, "Count" => count}),
    );
    let catalog_id = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .expect("synthetic PDF must serialize");
    bytes
}

fn snapshot(bytes: &[u8]) -> DocumentSnapshot {
    inspect(bytes, PdfLimits::default()).expect("synthetic PDF must inspect")
}

fn on_page(snapshot: &DocumentSnapshot, page: u32) -> Vec<&SourceFragment> {
    snapshot
        .fragments
        .iter()
        .filter(|f| f.page == page)
        .collect()
}

fn reasons(snapshot: &DocumentSnapshot, page: u32) -> &[String] {
    &snapshot.pages[page as usize - 1].reason_codes
}

fn has_reason(snapshot: &DocumentSnapshot, page: u32, code: &str) -> bool {
    reasons(snapshot, page).iter().any(|r| r == code)
}

/// Layout pairs whose label or value spans name one of `ids`.
fn pairs_touching(snapshot: &DocumentSnapshot, ids: &[&str]) -> usize {
    analyze(&snapshot.fragments)
        .pairs
        .iter()
        .filter(|pair| {
            std::iter::once(&pair.label)
                .chain(&pair.value)
                .any(|span| ids.contains(&span.fragment_id.as_str()))
        })
        .count()
}

/// A Form XObject drawing `runs` with its own `fonts`.
fn form(doc: &mut Document, fonts: Dictionary, runs: &[Run]) -> ObjectId {
    stream(
        doc,
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {"Font" => fonts},
        },
        operations(runs),
    )
}

fn draw(name: &str) -> Operation {
    Operation::new("Do", vec![Object::Name(name.into())])
}

/// Horizontal pages that must keep their pre-change snapshot: Type3 text with
/// label/value pairs; horizontal composite fonts (Identity-H, a predefined -H
/// CMap, an embedded CMap declaring `/WMode 0`); and a Form XObject.
fn horizontal_document() -> Vec<u8> {
    document(|doc| {
        let f1 = type3_font(doc);
        let identity_h = type0_font(doc, Object::Name(b"Identity-H".to_vec()));
        let uni_h = type0_font(doc, Object::Name(b"UniJIS-UCS2-H".to_vec()));
        let cmap = cmap_stream(doc, dictionary! {"WMode" => 0}, Some(0));
        let embedded_h = type0_font(doc, Object::Reference(cmap));
        let x1 = form(
            doc,
            dictionary! {"F2" => identity_h},
            &[composite("F2", 80.0, 600.0, "Form")],
        );
        let mut third = Page::new(
            dictionary! {"F1" => f1, "F4" => embedded_h},
            &[upright(80.0, 700.0, "Total: 2000")],
        );
        third.resources.set("XObject", dictionary! {"X1" => x1});
        third.operations.push(draw("X1"));
        vec![
            Page::new(
                dictionary! {"F1" => f1},
                &[
                    upright(80.0, 700.0, "Invoice No: A-1"),
                    upright(80.0, 660.0, "Total"),
                    upright(200.0, 660.0, "1000"),
                ],
            ),
            Page::new(
                dictionary! {"F1" => f1, "F2" => identity_h, "F3" => uni_h},
                &[
                    upright(80.0, 700.0, "Date: 2026-01-01"),
                    composite("F2", 80.0, 660.0, "Code"),
                ],
            ),
            third,
        ]
    })
}

type Row = (String, u32, String, [f64; 4], Quality);

fn rows(snapshot: &DocumentSnapshot) -> Vec<Row> {
    snapshot
        .fragments
        .iter()
        .map(|f| {
            (
                f.id.clone(),
                f.page,
                f.raw_text.clone(),
                [f.bbox.left, f.bbox.top, f.bbox.right, f.bbox.bottom],
                f.quality,
            )
        })
        .collect()
}

/// Captured from `inspect` before rotated-text and writing-mode handling
/// existed: upright pages must keep identical ids, text, boxes and quality.
#[test]
fn horizontal_pages_keep_their_snapshot() {
    let snapshot = snapshot(&horizontal_document());
    let expected: Vec<Row> = [
        (
            "b442b2865c67864bf5016576a784242f0c2d526afb42473531e6859cc4fb8f5f",
            1,
            "Invoice No: A-1",
            [80.0, 80.0, 188.0, 92.0],
        ),
        (
            "e73859bbf43b2692f2e639f46cfea40ab22bc97f78b45fb28669cf6ea2647a7a",
            1,
            "Total",
            [80.0, 120.0, 140.0, 132.0],
        ),
        (
            "d44fc527b72a6e61c689c5f994fc6aad12c306cb05e7102d5dd9eb52d0234351",
            1,
            "1000",
            [200.0, 120.0, 248.0, 132.0],
        ),
        (
            "741ad500419d7ff2eabd5a580e43f3894a2d1c4d926f2262663bf651a4739672",
            2,
            "Date: 2026-01-01",
            [80.0, 80.0, 195.20001220703125, 92.0],
        ),
        (
            "dc1724dbe0d7fb38ed2ab4f326af5dd584a26dc6e5de3d4ef3209b52aed805e1",
            2,
            "Code",
            [80.0, 120.0, 128.0, 132.0],
        ),
        (
            "1cd08c2f41d5e938694ca8454ce818c25961d2db2ef2361cf04f5fab228dea3f",
            3,
            "Total: 2000",
            [80.0, 80.0, 159.20001220703125, 92.0],
        ),
        (
            "36c889d4af4a64a8a003c6400466af963b5dbb0f75e3e02d1467ac5c20cc98d4",
            3,
            "Form",
            [80.0, 180.0, 128.0, 192.0],
        ),
    ]
    .into_iter()
    .map(|(id, page, text, bbox)| (id.into(), page, text.into(), bbox, Quality::Usable))
    .collect();
    assert_eq!(rows(&snapshot), expected);
    for page in &snapshot.pages {
        assert!(page.text_available, "page {}", page.page);
        assert_eq!(page.quality, Quality::Usable, "page {}", page.page);
        assert_eq!(
            page.reason_codes,
            [
                "native_geometry_unverified",
                "text_fidelity_and_occlusion_unverified",
                "upstream_internal_repair_count_unobservable",
            ],
            "page {}",
            page.page
        );
    }
    assert!(!analyze(&snapshot.fragments).pairs.is_empty());
}

/// Upright body text plus runs turned a quarter and a half turn, and a page
/// with one oblique run.
fn rotated_document() -> Vec<u8> {
    document(|doc| {
        let f1 = type3_font(doc);
        let body = || {
            vec![
                upright(80.0, 700.0, "Invoice No: A-1"),
                upright(80.0, 660.0, "Total"),
                upright(200.0, 660.0, "1000"),
                upright(80.0, 620.0, "Date: 2026-01-01"),
                upright(80.0, 580.0, "Name: Alice"),
            ]
        };
        let mut first = body();
        first.extend([
            turned([0.0, 1.0, -1.0, 0.0], 500.0, 300.0, "Ref: X-9"),
            turned([-1.0, 0.0, 0.0, -1.0], 400.0, 450.0, "Due: 2026"),
            turned([0.0, -1.0, 1.0, 0.0], 560.0, 500.0, "Memo: B-2"),
        ]);
        let (sin, cos) = 30f64.to_radians().sin_cos();
        let mut second = body();
        second.push(turned([cos, sin, -sin, cos], 300.0, 300.0, "Copy: Z-1"));
        vec![
            Page::new(dictionary! {"F1" => f1}, &first),
            Page::new(dictionary! {"F1" => f1}, &second),
        ]
    })
}

#[test]
fn quarter_and_half_turned_runs_are_unknown_and_never_paired() {
    let snapshot = snapshot(&rotated_document());
    let page = on_page(&snapshot, 1);
    let turned: Vec<&str> = page
        .iter()
        .filter(|f| ["Ref: X-9", "Due: 2026", "Memo: B-2"].contains(&f.raw_text.as_str()))
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(turned.len(), 3, "{page:?}");
    for fragment in &page {
        let expected = if turned.contains(&fragment.id.as_str()) {
            Quality::Unknown
        } else {
            // pdf-inspector never merges a run that is not upright into an
            // upright one, so the page's level text keeps its quality.
            Quality::Usable
        };
        assert_eq!(fragment.quality, expected, "{fragment:?}");
    }
    assert!(has_reason(&snapshot, 1, "rotated_text_present"));
    assert!(!has_reason(&snapshot, 1, "page_text_rotated"));
    assert_eq!(snapshot.pages[0].quality, Quality::Usable);
    assert!(snapshot.pages[0].text_available);
    assert_eq!(pairs_touching(&snapshot, &turned), 0);
    let level: Vec<&str> = page
        .iter()
        .filter(|f| !turned.contains(&f.id.as_str()))
        .map(|f| f.id.as_str())
        .collect();
    assert!(pairs_touching(&snapshot, &level) > 0);
}

#[test]
fn an_oblique_run_leaves_its_whole_page_unknown() {
    let snapshot = snapshot(&rotated_document());
    let page = on_page(&snapshot, 2);
    assert!(page.len() >= 5, "{page:?}");
    assert!(
        page.iter().all(|f| f.quality == Quality::Unknown),
        "{page:?}"
    );
    assert!(has_reason(&snapshot, 2, "oblique_text_present"));
    assert!(has_reason(&snapshot, 2, "no_usable_source_runs"));
    assert_eq!(snapshot.pages[1].quality, Quality::Unknown);
    assert!(!snapshot.pages[1].text_available);
    let ids: Vec<&str> = page.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(pairs_touching(&snapshot, &ids), 0);
}

/// Mostly bottom-to-top runs, so pdf-inspector re-bases the page frame; one
/// upright stray reads left to right on the rendered page.
fn rebased_document() -> Vec<u8> {
    document(|doc| {
        let f1 = type3_font(doc);
        let up = [0.0, 1.0, -1.0, 0.0];
        vec![Page::new(
            dictionary! {"F1" => f1},
            &[
                turned(up, 100.0, 100.0, "Invoice No: A-1"),
                turned(up, 140.0, 100.0, "Total: 1000"),
                turned(up, 180.0, 100.0, "Name: Alice"),
                turned(up, 220.0, 100.0, "Memo: B-2"),
                upright(300.0, 700.0, "Date: 2026-01-01"),
            ],
        )]
    })
}

#[test]
fn a_rebased_page_is_unknown_as_a_whole() {
    let bytes = rebased_document();
    let (_, turns) = pdf_inspector::extract_text_with_positions_and_rotations_mem_in_frame(
        &bytes,
        None,
        pdf_inspector::PositionFrame::Display,
    )
    .expect("synthetic PDF must extract");
    assert_eq!(turns.get(&1), Some(&pdf_inspector::PageRotation::Ccw));
    let snapshot = snapshot(&bytes);
    let page = on_page(&snapshot, 1);
    assert_eq!(page.len(), 5, "{page:?}");
    assert!(
        page.iter().all(|f| f.quality == Quality::Unknown),
        "{page:?}"
    );
    assert!(has_reason(&snapshot, 1, "page_text_rotated"));
    assert!(has_reason(&snapshot, 1, "rotated_text_present"));
    assert!(!snapshot.pages[0].text_available);
    let ids: Vec<&str> = page.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(pairs_touching(&snapshot, &ids), 0);
}

/// The same text as a landscape page drawn upright, or as a portrait sheet
/// with `/Rotate 90` whose content is turned to read upright once rendered.
fn landscape_document(rotated_sheet: bool) -> Vec<u8> {
    document(|doc| {
        let f1 = type3_font(doc);
        let lines = [
            (80.0, 500.0, "Invoice No: A-1"),
            (80.0, 460.0, "Total"),
            (200.0, 460.0, "1000"),
            (80.0, 420.0, "Date: 2026-01-01"),
        ];
        let runs: Vec<Run> = lines
            .iter()
            .map(|&(u, v, text)| {
                if rotated_sheet {
                    // Landscape (u, v) sits at sheet (612 - v, u).
                    turned([0.0, 1.0, -1.0, 0.0], 612.0 - v, u, text)
                } else {
                    upright(u, v, text)
                }
            })
            .collect();
        let mut page = Page::new(dictionary! {"F1" => f1}, &runs);
        if rotated_sheet {
            page.rotate = Some(90);
        } else {
            page.media = [0, 0, 792, 612];
        }
        vec![page]
    })
}

#[test]
fn a_rebased_frame_matching_page_rotate_reads_like_an_upright_page() {
    let rotated = landscape_document(true);
    let (_, turns) = pdf_inspector::extract_text_with_positions_and_rotations_mem_in_frame(
        &rotated,
        None,
        pdf_inspector::PositionFrame::Display,
    )
    .expect("synthetic PDF must extract");
    // pdf-inspector re-based the sheet, the same quarter turn as /Rotate 90.
    assert_eq!(turns.get(&1), Some(&pdf_inspector::PageRotation::Ccw));
    let rotated = snapshot(&rotated);
    let upright = snapshot(&landscape_document(false));
    assert_eq!(rotated.pages[0].reason_codes, upright.pages[0].reason_codes);
    assert_eq!(rotated.pages[0].quality, Quality::Usable);
    assert_eq!(rotated.fragments.len(), upright.fragments.len());
    assert_eq!(rotated.fragments.len(), 4);
    for (r, u) in rotated.fragments.iter().zip(&upright.fragments) {
        assert_eq!(r.raw_text, u.raw_text);
        assert_eq!(r.quality, Quality::Usable);
        assert_eq!(u.quality, Quality::Usable);
        for (a, b) in [
            (r.bbox.left, u.bbox.left),
            (r.bbox.top, u.bbox.top),
            (r.bbox.right, u.bbox.right),
            (r.bbox.bottom, u.bbox.bottom),
        ] {
            assert!((a - b).abs() < 0.01, "{r:?} vs {u:?}");
        }
    }
    let pairs = |s: &DocumentSnapshot| {
        analyze(&s.fragments)
            .pairs
            .iter()
            .map(|p| (p.label.start, p.label.end, p.value.len(), p.placement))
            .collect::<Vec<_>>()
    };
    assert_eq!(pairs(&rotated), pairs(&upright));
}

/// Upright Type3 body text on a page whose resources also hold `font` as F2,
/// which draws one run (directly, or through a Form XObject).
fn vertical_font_document(font: impl FnOnce(&mut Document) -> ObjectId, in_form: bool) -> Vec<u8> {
    document(|doc| {
        let f1 = type3_font(doc);
        let f2 = font(doc);
        let body = [
            upright(80.0, 700.0, "Invoice No: A-1"),
            upright(80.0, 660.0, "Total"),
            upright(200.0, 660.0, "1000"),
        ];
        let vertical = composite("F2", 400.0, 600.0, "Code");
        if in_form {
            let x1 = form(doc, dictionary! {"F2" => f2}, &[vertical]);
            let mut page = Page::new(dictionary! {"F1" => f1}, &body);
            page.resources.set("XObject", dictionary! {"X1" => x1});
            page.operations.push(draw("X1"));
            vec![page]
        } else {
            let mut runs = Vec::from(body);
            runs.push(vertical);
            vec![Page::new(dictionary! {"F1" => f1, "F2" => f2}, &runs)]
        }
    })
}

fn assert_page_unknown(snapshot: &DocumentSnapshot, code: &str, case: &str) {
    let page = on_page(snapshot, 1);
    assert!(page.len() >= 3, "{case}: {page:?}");
    assert!(
        page.iter().all(|f| f.quality != Quality::Usable),
        "{case}: {page:?}"
    );
    assert!(
        has_reason(snapshot, 1, code),
        "{case}: {:?}",
        reasons(snapshot, 1)
    );
    assert_ne!(snapshot.pages[0].quality, Quality::Usable, "{case}");
    assert!(!snapshot.pages[0].text_available, "{case}");
    assert!(analyze(&snapshot.fragments).pairs.is_empty(), "{case}");
}

#[test]
fn vertical_writing_mode_fonts_leave_the_page_unknown() {
    let name = |name: &'static str| {
        move |doc: &mut Document| type0_font(doc, Object::Name(name.as_bytes().to_vec()))
    };
    let embedded = |dict: fn() -> Dictionary, wmode: Option<u8>| {
        move |doc: &mut Document| {
            let cmap = cmap_stream(doc, dict(), wmode);
            type0_font(doc, Object::Reference(cmap))
        }
    };
    type Case = (
        &'static str,
        Box<dyn FnOnce(&mut Document) -> ObjectId>,
        bool,
    );
    let cases: Vec<Case> = vec![
        ("Identity-V", Box::new(name("Identity-V")), false),
        ("predefined -V CMap", Box::new(name("UniJIS-UCS2-V")), false),
        ("RKSJ -V CMap", Box::new(name("90ms-RKSJ-V")), false),
        ("bare V CMap", Box::new(name("V")), false),
        (
            "CMap dictionary WMode",
            Box::new(embedded(|| dictionary! {"WMode" => 1}, None)),
            false,
        ),
        (
            "CMap program WMode",
            Box::new(embedded(Dictionary::new, Some(1))),
            false,
        ),
        (
            "UseCMap -V",
            Box::new(embedded(|| dictionary! {"UseCMap" => "Identity-V"}, None)),
            false,
        ),
        ("Form XObject font", Box::new(name("Identity-V")), true),
    ];
    for (case, font, in_form) in cases {
        let snapshot = snapshot(&vertical_font_document(font, in_form));
        assert_page_unknown(&snapshot, "vertical_writing_mode_unsupported", case);
    }
}

#[test]
fn an_unfinished_font_scan_leaves_the_page_unknown() {
    // Form XObjects nested deeper than the scan follows (listed, never drawn).
    let deep = document(|doc| {
        let f1 = type3_font(doc);
        let mut inner = stream(doc, dictionary! {"Subtype" => "Form"}, vec![]);
        for _ in 0..16 {
            inner = stream(
                doc,
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
                    "Resources" => dictionary! {"XObject" => dictionary! {"X" => inner}},
                },
                vec![],
            );
        }
        let mut page = Page::new(
            dictionary! {"F1" => f1},
            &[
                upright(80.0, 700.0, "Invoice No: A-1"),
                upright(80.0, 660.0, "Total"),
                upright(200.0, 660.0, "1000"),
            ],
        );
        page.resources.set("XObject", dictionary! {"X" => inner});
        vec![page]
    });
    assert_page_unknown(&snapshot(&deep), "writing_mode_scan_incomplete", "depth");

    // More font entries than one page's scan budget.
    let crowded = document(|doc| {
        let f1 = type3_font(doc);
        let mut fonts = dictionary! {"F1" => f1};
        for index in 0..5000 {
            fonts.set(format!("G{index}"), f1);
        }
        vec![Page::new(
            fonts,
            &[
                upright(80.0, 700.0, "Invoice No: A-1"),
                upright(80.0, 660.0, "Total"),
                upright(200.0, 660.0, "1000"),
            ],
        )]
    });
    assert_page_unknown(&snapshot(&crowded), "writing_mode_scan_incomplete", "nodes");
}

#[test]
fn self_referencing_form_resources_finish_the_scan() {
    let bytes = document(|doc| {
        let f1 = type3_font(doc);
        let id = doc.new_object_id();
        doc.objects.insert(
            id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
                    "Resources" => dictionary! {"XObject" => dictionary! {"X" => id}},
                },
                vec![],
            )),
        );
        let mut page = Page::new(
            dictionary! {"F1" => f1},
            &[upright(80.0, 700.0, "Invoice No: A-1")],
        );
        page.resources.set("XObject", dictionary! {"X" => id});
        vec![page]
    });
    let snapshot = snapshot(&bytes);
    assert_eq!(snapshot.pages[0].quality, Quality::Usable);
    assert!(!has_reason(&snapshot, 1, "writing_mode_scan_incomplete"));
    assert!(
        snapshot
            .fragments
            .iter()
            .all(|f| f.quality == Quality::Usable)
    );
}

#[test]
fn upright_content_on_a_rotated_page_is_unknown() {
    // Upright on the sheet, so pdf-inspector does not re-base the page; the
    // renderer turns it by /Rotate 90 and every run reads top to bottom.
    let bytes = document(|doc| {
        let f1 = type3_font(doc);
        let mut page = Page::new(
            dictionary! {"F1" => f1},
            &[
                upright(80.0, 700.0, "Invoice No: A-1"),
                upright(80.0, 660.0, "Total"),
                upright(200.0, 660.0, "1000"),
            ],
        );
        page.rotate = Some(90);
        vec![page]
    });
    let (_, turns) = pdf_inspector::extract_text_with_positions_and_rotations_mem_in_frame(
        &bytes,
        None,
        pdf_inspector::PositionFrame::Display,
    )
    .expect("synthetic PDF must extract");
    assert!(turns.is_empty(), "{turns:?}");
    let snapshot = snapshot(&bytes);
    let page = on_page(&snapshot, 1);
    assert_eq!(page.len(), 3, "{page:?}");
    assert!(
        page.iter().all(|f| f.quality == Quality::Unknown),
        "{page:?}"
    );
    assert!(has_reason(&snapshot, 1, "rotated_text_present"));
    assert!(has_reason(&snapshot, 1, "no_usable_source_runs"));
    assert!(!has_reason(&snapshot, 1, "page_text_rotated"));
    assert_eq!(snapshot.pages[0].quality, Quality::Unknown);
    assert!(!snapshot.pages[0].text_available);
    assert!(analyze(&snapshot.fragments).pairs.is_empty());
}
