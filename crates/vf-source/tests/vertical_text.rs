//! Right-angle-turned text is read in its own direction; oblique text,
//! re-based pages and vertical writing mode fonts never reach layout as
//! usable source text. PDFs are built with lopdf; Type3 glyphs need no font
//! program.
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};
use vf_source::geometry::Rect;
use vf_source::layout::{Binding, Layout, LayoutLimits, Pair, Placement, Precision, Span, analyze};
use vf_source::{DocumentSnapshot, Orientation, PdfLimits, Quality, SourceFragment, inspect};

/// One text show: font resource tag, `Tf` size, text matrix and the shown
/// string.
struct Run {
    font: &'static str,
    size: i64,
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
        size: 12,
        matrix: [a, b, c, d, x, y],
        text: Object::string_literal(text),
    }
}

/// A run in a two-byte composite font whose codes are the ASCII codes.
fn composite(font: &'static str, x: f64, y: f64, text: &str) -> Run {
    composite_turned(font, [1.0, 0.0, 0.0, 1.0], x, y, text)
}

/// A composite-font run whose codes are the characters' (BMP) code points.
fn composite_turned(font: &'static str, linear: [f64; 4], x: f64, y: f64, text: &str) -> Run {
    let [a, b, c, d] = linear;
    let bytes = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
    Run {
        font,
        size: 12,
        matrix: [a, b, c, d, x, y],
        text: Object::String(bytes, StringFormat::Hexadecimal),
    }
}

/// Text matrices turning the baseline by a quarter or half turn
/// counter-clockwise: 90° reads bottom to top, 270° top to bottom.
const DEG90: [f64; 4] = [0.0, 1.0, -1.0, 0.0];
const DEG180: [f64; 4] = [-1.0, 0.0, 0.0, -1.0];
const DEG270: [f64; 4] = [0.0, -1.0, 1.0, 0.0];

fn operations(runs: &[Run]) -> Vec<Operation> {
    let mut operations = Vec::new();
    for run in runs {
        operations.push(Operation::new("BT", vec![]));
        operations.push(Operation::new(
            "Tf",
            vec![Object::Name(run.font.into()), run.size.into()],
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
    type0_font_with(doc, encoding, "")
}

/// [`type0_font`] whose ToUnicode also maps the code of each (BMP)
/// character in `extra` to that character.
fn type0_font_with(doc: &mut Document, encoding: Object, extra: &str) -> ObjectId {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> endcodespacerange\n",
    );
    cmap += &format!("{} beginbfchar\n", CHARSET.len() + extra.chars().count());
    for c in CHARSET.chars().chain(extra.chars()) {
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

/// Layout with the default limits, which these small pages never reach.
fn analyzed(fragments: &[SourceFragment]) -> Layout {
    analyze(fragments, &LayoutLimits::default()).expect("layout within the default limits")
}

/// Layout pairs whose label or value spans name one of `ids`.
fn pairs_touching(snapshot: &DocumentSnapshot, ids: &[&str]) -> usize {
    analyzed(&snapshot.fragments)
        .pairs
        .iter()
        .filter(|pair| {
            std::iter::once(&pair.label)
                .chain(&pair.value)
                .any(|span| ids.contains(&span.fragment_id.as_str()))
        })
        .count()
}

/// The fragment on `page` whose text is `text`.
fn fragment<'a>(snapshot: &'a DocumentSnapshot, page: u32, text: &str) -> &'a SourceFragment {
    on_page(snapshot, page)
        .into_iter()
        .find(|f| f.raw_text == text)
        .unwrap_or_else(|| panic!("no {text:?} on page {page}: {:?}", on_page(snapshot, page)))
}

fn by_id<'a>(snapshot: &'a DocumentSnapshot, id: &str) -> &'a SourceFragment {
    snapshot
        .fragments
        .iter()
        .find(|f| f.id == id)
        .expect("spans name existing fragments")
}

fn span_text<'a>(snapshot: &'a DocumentSnapshot, span: &Span) -> &'a str {
    &by_id(snapshot, &span.fragment_id).raw_text[span.start..span.end]
}

/// The pairs whose label or value spans name `fragment`.
fn pairs_of<'a>(layout: &'a Layout, fragment: &SourceFragment) -> Vec<&'a Pair> {
    layout
        .pairs
        .iter()
        .filter(|pair| {
            std::iter::once(&pair.label)
                .chain(&pair.value)
                .any(|span| span.fragment_id == fragment.id)
        })
        .collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// Every span lies inside its own fragment's Display box, and every pair
/// and line keeps to one orientation.
fn assert_layout_consistent(snapshot: &DocumentSnapshot, layout: &Layout) {
    let inside = |outer: Rect, inner: Rect| {
        inner.validate().is_ok()
            && inner.left >= outer.left - 1e-9
            && inner.top >= outer.top - 1e-9
            && inner.right <= outer.right + 1e-9
            && inner.bottom <= outer.bottom + 1e-9
    };
    for pair in &layout.pairs {
        let orientation = by_id(snapshot, &pair.label.fragment_id).orientation;
        for span in std::iter::once(&pair.label).chain(&pair.value) {
            let run = by_id(snapshot, &span.fragment_id);
            assert_eq!(run.page, pair.page, "{pair:?}");
            assert_eq!(run.orientation, orientation, "{pair:?}");
            assert!(inside(run.bbox, span.bbox), "{span:?} outside {run:?}");
            if span.precision == Precision::Run {
                assert_eq!(span.bbox, run.bbox, "{span:?}");
            }
        }
    }
    for line in &layout.lines {
        let first = &snapshot.fragments[line.fragments[0]];
        for &index in &line.fragments {
            let run = &snapshot.fragments[index];
            assert_eq!(run.page, line.page, "{line:?}");
            assert_eq!(run.orientation, first.orientation, "{line:?}");
            assert_eq!(run.quality, Quality::Usable, "{line:?}");
            assert!(inside(line.bbox, run.bbox), "{run:?} outside {line:?}");
        }
    }
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
/// Ids hash the whole PDF, which lopdf serializes here, so a change in its
/// writer moves them; lopdf 0.47 writes whole-number reals as `80.0`.
#[test]
fn horizontal_pages_keep_their_snapshot() {
    let snapshot = snapshot(&horizontal_document());
    let expected: Vec<Row> = [
        (
            "7e2b69bd00f8ed970dc11618ce7be3216fee055cd23904c87ac14b7363387451",
            1,
            "Invoice No: A-1",
            [80.0, 80.0, 188.0, 92.0],
        ),
        (
            "04d4dcd8132107cfc2d4b7dbd371520a60f16ebcc312fa781cffb85abc7a4744",
            1,
            "Total",
            [80.0, 120.0, 140.0, 132.0],
        ),
        (
            "160cc815027009e2ca84701141b17c0d8fe9c05a9fb55855335ab82e93e50f07",
            1,
            "1000",
            [200.0, 120.0, 248.0, 132.0],
        ),
        (
            "f566d40299c5c83c051b9c3da1cdcf62ce12e71287d9927136d9b730cfec602d",
            2,
            "Date: 2026-01-01",
            [80.0, 80.0, 195.20001220703125, 92.0],
        ),
        (
            "aba632a17aacef711e8d26cb69ab8ca1bf76f8bae0ec3afe798db994e2fb8f62",
            2,
            "Code",
            [80.0, 120.0, 128.0, 132.0],
        ),
        (
            "b28e377e20e561c64dd165f4242374a81eb919c103cc4121bc6ede881aa51d29",
            3,
            "Total: 2000",
            [80.0, 80.0, 159.20001220703125, 92.0],
        ),
        (
            "b6ecac8d3d749c6301c4d8907f5598bfdf0fd9fd62645ffcbb24240c0231239d",
            3,
            "Form",
            [80.0, 180.0, 128.0, 192.0],
        ),
    ]
    .into_iter()
    .map(|(id, page, text, bbox)| (id.into(), page, text.into(), bbox, Quality::Usable))
    .collect();
    assert_eq!(rows(&snapshot), expected);
    // Upright fragments serialize without an orientation key.
    let json = serde_json::to_value(&snapshot.fragments).expect("fragments serialize");
    for fragment in json.as_array().expect("an array of fragments") {
        let mut keys: Vec<&str> = fragment
            .as_object()
            .expect("a fragment object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["bbox", "id", "item_index", "page", "quality", "raw_text"]
        );
    }
    assert!(
        snapshot
            .fragments
            .iter()
            .all(|f| f.orientation == Orientation::Deg0)
    );
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
    assert!(!analyzed(&snapshot.fragments).pairs.is_empty());
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
            turned(DEG90, 500.0, 300.0, "Ref: X-9"),
            turned(DEG180, 400.0, 450.0, "Due: 2026"),
            turned(DEG270, 560.0, 500.0, "Memo: B-2"),
        ]);
        let (sin, cos) = 30f64.to_radians().sin_cos();
        let mut second = body();
        second.push(turned([cos, sin, -sin, cos], 300.0, 300.0, "Copy: Z-1"));
        second.push(turned(DEG90, 500.0, 300.0, "Ref: X-9"));
        vec![
            Page::new(dictionary! {"F1" => f1}, &first),
            Page::new(dictionary! {"F1" => f1}, &second),
        ]
    })
}

#[test]
fn quarter_and_half_turned_runs_pair_in_their_own_frame() {
    let snapshot = snapshot(&rotated_document());
    let turned = [
        ("Ref: X-9", Orientation::Deg90, "Ref", "X-9"),
        ("Due: 2026", Orientation::Deg180, "Due", "2026"),
        ("Memo: B-2", Orientation::Deg270, "Memo", "B-2"),
    ];
    let page = on_page(&snapshot, 1);
    assert_eq!(page.len(), 8, "{page:?}");
    for f in &page {
        assert_eq!(f.quality, Quality::Usable, "{f:?}");
        let expected = turned
            .iter()
            .find(|(text, ..)| *text == f.raw_text)
            .map_or(Orientation::Deg0, |turn| turn.1);
        assert_eq!(f.orientation, expected, "{f:?}");
    }
    assert!(has_reason(&snapshot, 1, "rotated_text_present"));
    assert!(!has_reason(&snapshot, 1, "page_text_rotated"));
    assert!(!has_reason(&snapshot, 1, "rotated_script_join_unverified"));
    assert_eq!(snapshot.pages[0].quality, Quality::Usable);
    assert!(snapshot.pages[0].text_available);
    let layout = analyzed(&snapshot.fragments);
    assert_layout_consistent(&snapshot, &layout);
    for (text, orientation, label, value) in turned {
        let run = fragment(&snapshot, 1, text);
        // A quarter-turned run's box is one em wide and its advance tall.
        assert_eq!(
            run.bbox.height() > run.bbox.width(),
            orientation != Orientation::Deg180,
            "{run:?}"
        );
        let pairs = pairs_of(&layout, run);
        assert_eq!(pairs.len(), 1, "{text}: {pairs:?}");
        let pair = pairs[0];
        assert_eq!(pair.placement, Placement::Inline, "{text}");
        assert_eq!(span_text(&snapshot, &pair.label), label);
        assert_eq!(pair.value.len(), 1, "{text}");
        assert_eq!(span_text(&snapshot, &pair.value[0]), value);
        assert_eq!(pair.label.precision, Precision::Estimated);
        assert_eq!(pair.value[0].precision, Precision::Estimated);
        // The label starts the run in its reading direction and the value
        // ends it; across that direction both keep the run's extent.
        let (r, l, v) = (run.bbox, pair.label.bbox, pair.value[0].bbox);
        match orientation {
            Orientation::Deg90 => {
                assert!(close(l.bottom, r.bottom) && close(v.top, r.top));
                assert!(v.bottom <= l.top + 1e-9, "{l:?} {v:?}");
                assert!(close(l.left, r.left) && close(v.right, r.right));
            }
            Orientation::Deg180 => {
                assert!(close(l.right, r.right) && close(v.left, r.left));
                assert!(v.right <= l.left + 1e-9, "{l:?} {v:?}");
                assert!(close(l.top, r.top) && close(v.bottom, r.bottom));
            }
            Orientation::Deg270 => {
                assert!(close(l.top, r.top) && close(v.bottom, r.bottom));
                assert!(v.top >= l.bottom - 1e-9, "{l:?} {v:?}");
                assert!(close(l.left, r.left) && close(v.right, r.right));
            }
            Orientation::Deg0 => unreachable!(),
        }
    }
    // The level text pairs as before, never with a turned run.
    let level: Vec<&str> = page
        .iter()
        .filter(|f| f.orientation == Orientation::Deg0)
        .map(|f| f.id.as_str())
        .collect();
    assert!(pairs_touching(&snapshot, &level) > 0);
}

#[test]
fn an_oblique_run_leaves_its_whole_page_unknown() {
    let snapshot = snapshot(&rotated_document());
    let page = on_page(&snapshot, 2);
    assert!(page.len() >= 6, "{page:?}");
    assert!(
        page.iter().all(|f| f.quality == Quality::Unknown),
        "{page:?}"
    );
    // A right-angle-turned run on the page is held back with it.
    assert_eq!(
        fragment(&snapshot, 2, "Ref: X-9").orientation,
        Orientation::Deg90
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
    // Display orientations: the dominant runs read bottom to top, the stray
    // left to right.
    for f in &page {
        let expected = if f.raw_text.starts_with("Date") {
            Orientation::Deg0
        } else {
            Orientation::Deg90
        };
        assert_eq!(f.orientation, expected, "{f:?}");
    }
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
        analyzed(&s.fragments)
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
            turned(DEG90, 500.0, 300.0, "Ref: X-9"),
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
    assert!(page.len() >= 4, "{case}: {page:?}");
    // The page-level verdict covers its turned run too.
    assert!(
        page.iter().any(|f| f.orientation == Orientation::Deg90),
        "{case}: {page:?}"
    );
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
    assert!(analyzed(&snapshot.fragments).pairs.is_empty(), "{case}");
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
                turned(DEG90, 500.0, 300.0, "Ref: X-9"),
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
                turned(DEG90, 500.0, 300.0, "Ref: X-9"),
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

/// Body text upright on the sheet, on a page with `/Rotate 90` or not.
fn sheet_document(rotate: Option<i64>) -> Vec<u8> {
    document(|doc| {
        let f1 = type3_font(doc);
        let mut page = Page::new(
            dictionary! {"F1" => f1},
            &[
                upright(80.0, 700.0, "Invoice No: A-1"),
                upright(80.0, 660.0, "Total"),
                upright(200.0, 660.0, "1000"),
            ],
        );
        page.rotate = rotate;
        vec![page]
    })
}

/// Label text, value texts and placement of every pair on `page`.
fn signature(snapshot: &DocumentSnapshot, page: u32) -> Vec<(String, Vec<String>, Placement)> {
    analyzed(&snapshot.fragments)
        .pairs
        .iter()
        .filter(|pair| pair.page == page)
        .map(|pair| {
            (
                span_text(snapshot, &pair.label).to_owned(),
                pair.value
                    .iter()
                    .map(|span| span_text(snapshot, span).to_owned())
                    .collect(),
                pair.placement,
            )
        })
        .collect()
}

#[test]
fn upright_content_on_a_rotated_page_reads_in_its_own_frame() {
    // Upright on the sheet, so pdf-inspector does not re-base the page and
    // merges it as an upright page; the renderer turns it by /Rotate 90 and
    // every run reads top to bottom.
    let bytes = sheet_document(Some(90));
    let (_, turns) = pdf_inspector::extract_text_with_positions_and_rotations_mem_in_frame(
        &bytes,
        None,
        pdf_inspector::PositionFrame::Display,
    )
    .expect("synthetic PDF must extract");
    assert!(turns.is_empty(), "{turns:?}");
    let rotated = snapshot(&bytes);
    let upright = snapshot(&sheet_document(None));
    let page = on_page(&rotated, 1);
    assert_eq!(page.len(), 3, "{page:?}");
    for (r, u) in page.iter().zip(on_page(&upright, 1)) {
        assert_eq!(r.raw_text, u.raw_text);
        assert_eq!(r.quality, Quality::Usable, "{r:?}");
        assert_eq!(r.orientation, Orientation::Deg270, "{r:?}");
        // The sheet turned clockwise: widths become heights.
        assert!(
            (r.bbox.height() - u.bbox.width()).abs() < 0.01,
            "{r:?} {u:?}"
        );
        assert!(
            (r.bbox.width() - u.bbox.height()).abs() < 0.01,
            "{r:?} {u:?}"
        );
    }
    assert!(has_reason(&rotated, 1, "rotated_text_present"));
    assert!(!has_reason(&rotated, 1, "no_usable_source_runs"));
    assert!(!has_reason(&rotated, 1, "page_text_rotated"));
    assert_eq!(rotated.pages[0].quality, Quality::Usable);
    assert!(rotated.pages[0].text_available);
    let layout = analyzed(&rotated.fragments);
    assert_layout_consistent(&rotated, &layout);
    assert_eq!(layout.lines.len(), 2, "{:?}", layout.lines);
    let expected = signature(&upright, 1);
    assert_eq!(
        expected,
        [
            ("Invoice No".into(), vec!["A-1".into()], Placement::Inline),
            ("Total".into(), vec!["1000".into()], Placement::Right),
        ]
    );
    assert_eq!(signature(&rotated, 1), expected);
}

/// `請求番号：INV-001` in a composite font, turned a quarter turn on page 1
/// and upright on page 2, each beside upright body text.
fn japanese_document() -> Vec<u8> {
    document(|doc| {
        let f1 = type3_font(doc);
        let f2 = type0_font_with(doc, Object::Name(b"Identity-H".to_vec()), "請求番号：");
        let fonts = dictionary! {"F1" => f1, "F2" => f2};
        let body = || {
            vec![
                upright(80.0, 700.0, "Invoice No: A-1"),
                upright(80.0, 660.0, "Total"),
                upright(200.0, 660.0, "1000"),
            ]
        };
        let mut turned = body();
        turned.push(composite_turned(
            "F2",
            DEG90,
            500.0,
            200.0,
            "請求番号：INV-001",
        ));
        let mut level = body();
        level.push(composite("F2", 300.0, 400.0, "請求番号：INV-001"));
        vec![Page::new(fonts.clone(), &turned), Page::new(fonts, &level)]
    })
}

#[test]
fn a_quarter_turned_japanese_label_pairs_at_its_delimiter() {
    let snapshot = snapshot(&japanese_document());
    let run = fragment(&snapshot, 1, "請求番号：INV-001");
    let twin = fragment(&snapshot, 2, "請求番号：INV-001");
    assert_eq!(run.quality, Quality::Usable, "{run:?}");
    assert_eq!(run.orientation, Orientation::Deg90);
    assert_eq!(twin.quality, Quality::Usable, "{twin:?}");
    assert_eq!(twin.orientation, Orientation::Deg0);
    assert!(snapshot.pages.iter().all(|p| p.quality == Quality::Usable));
    // Display box: the baseline starts at (500, 200) and climbs; glyph tops
    // face left, so the run is one em wide left of x = 500.
    assert!((run.bbox.right - 500.0).abs() < 0.01, "{run:?}");
    assert!((run.bbox.bottom - (792.0 - 200.0)).abs() < 0.01, "{run:?}");
    assert!((run.bbox.width() - twin.bbox.height()).abs() < 0.01);
    assert!((run.bbox.height() - twin.bbox.width()).abs() < 0.01);
    assert!(run.bbox.height() > 5.0 * run.bbox.width(), "{run:?}");

    let layout = analyzed(&snapshot.fragments);
    assert_layout_consistent(&snapshot, &layout);
    let turned = pairs_of(&layout, run);
    let level = pairs_of(&layout, twin);
    assert_eq!(turned.len(), 1, "{turned:?}");
    assert_eq!(level.len(), 1, "{level:?}");
    let (pair, twin_pair) = (turned[0], level[0]);
    assert_eq!(pair.placement, Placement::Inline);
    assert_eq!(span_text(&snapshot, &pair.label), "請求番号");
    assert_eq!(span_text(&snapshot, &pair.value[0]), "INV-001");
    assert_eq!(
        (
            pair.label.start,
            pair.label.end,
            pair.value[0].start,
            pair.value[0].end
        ),
        (
            twin_pair.label.start,
            twin_pair.label.end,
            twin_pair.value[0].start,
            twin_pair.value[0].end
        )
    );
    // Evidence spans: the label from the run's bottom, the value up to its
    // top, at the same offsets along the run as the upright twin's spans.
    let along = |span: &Span| {
        (
            run.bbox.bottom - span.bbox.bottom,
            run.bbox.bottom - span.bbox.top,
        )
    };
    let twin_along = |span: &Span| {
        (
            span.bbox.left - twin.bbox.left,
            span.bbox.right - twin.bbox.left,
        )
    };
    for (span, twin_span) in [
        (&pair.label, &twin_pair.label),
        (&pair.value[0], &twin_pair.value[0]),
    ] {
        let ((a0, a1), (b0, b1)) = (along(span), twin_along(twin_span));
        assert!((a0 - b0).abs() < 0.01 && (a1 - b1).abs() < 0.01, "{span:?}");
        assert!(close(span.bbox.left, run.bbox.left) && close(span.bbox.right, run.bbox.right));
    }
    assert!(close(pair.label.bbox.bottom, run.bbox.bottom));
    assert!(close(pair.value[0].bbox.top, run.bbox.top));
    assert!(pair.value[0].bbox.bottom <= pair.label.bbox.top + 1e-9);
}

/// A text matrix's linear part and the origins of `Total`, `1000`, `Name`
/// and `Alice`.
type TurnedPage = ([f64; 4], [(f64, f64); 4]);

/// Per orientation, `Total` then `1000` on one line and `Name` over `Alice`
/// on the next line, as read in that orientation; each page also holds
/// upright body text. Pages: 90°, 180°, 270°.
fn separate_runs_document() -> Vec<u8> {
    document(|doc| {
        let f1 = type3_font(doc);
        let layouts: [TurnedPage; 3] = [
            // Bottom to top: the next line lies to the right.
            (
                DEG90,
                [(60.0, 200.0), (60.0, 320.0), (160.0, 300.0), (174.0, 300.0)],
            ),
            // Right to left: the next line lies above.
            (
                DEG180,
                [
                    (400.0, 200.0),
                    (300.0, 200.0),
                    (400.0, 300.0),
                    (400.0, 314.0),
                ],
            ),
            // Top to bottom: the next line lies to the left.
            (
                DEG270,
                [
                    (520.0, 600.0),
                    (520.0, 480.0),
                    (420.0, 300.0),
                    (406.0, 300.0),
                ],
            ),
        ];
        layouts
            .iter()
            .map(|(linear, at)| {
                let mut runs = vec![
                    upright(80.0, 740.0, "Invoice No: A-1"),
                    upright(80.0, 720.0, "Date: 2026-01-01"),
                    upright(300.0, 740.0, "Code: K-7"),
                    upright(300.0, 720.0, "Kind: Q"),
                    upright(80.0, 60.0, "Page 1"),
                ];
                for ((x, y), text) in at.iter().zip(["Total", "1000", "Name", "Alice"]) {
                    runs.push(turned(*linear, *x, *y, text));
                }
                Page::new(dictionary! {"F1" => f1}, &runs)
            })
            .collect()
    })
}

#[test]
fn turned_label_and_value_runs_pair_in_reading_order() {
    let snapshot = snapshot(&separate_runs_document());
    let layout = analyzed(&snapshot.fragments);
    assert_layout_consistent(&snapshot, &layout);
    let orientations = [Orientation::Deg90, Orientation::Deg180, Orientation::Deg270];
    for (page, orientation) in (1..).zip(orientations) {
        assert_eq!(snapshot.pages[page as usize - 1].quality, Quality::Usable);
        let get = |text| fragment(&snapshot, page, text);
        let (total, amount, name, alice) = (get("Total"), get("1000"), get("Name"), get("Alice"));
        for f in [total, amount, name, alice] {
            assert_eq!(f.quality, Quality::Usable, "{f:?}");
            assert_eq!(f.orientation, orientation, "{f:?}");
        }
        let turned: Vec<&Pair> = layout
            .pairs
            .iter()
            .filter(|p| {
                p.page == page && by_id(&snapshot, &p.label.fragment_id).orientation == orientation
            })
            .collect();
        assert_eq!(turned.len(), 2, "{orientation:?}: {turned:?}");
        let right = turned
            .iter()
            .find(|p| p.placement == Placement::Right)
            .expect("a right pair");
        let below = turned
            .iter()
            .find(|p| p.placement == Placement::Below)
            .expect("a below pair");
        assert_eq!(right.label.fragment_id, total.id);
        assert_eq!(right.value.len(), 1);
        assert_eq!(right.value[0].fragment_id, amount.id);
        assert_eq!(below.label.fragment_id, name.id);
        assert_eq!(below.value.len(), 1);
        assert_eq!(below.value[0].fragment_id, alice.id);
        for span in [&right.label, &right.value[0], &below.label, &below.value[0]] {
            assert_eq!(span.precision, Precision::Run, "{span:?}");
        }
        // The value follows in the reading direction; the next line lies on
        // the side the glyph bottoms face.
        let (t, a, n, l) = (total.bbox, amount.bbox, name.bbox, alice.bbox);
        match orientation {
            Orientation::Deg90 => assert!(a.bottom <= t.top && l.left >= n.right),
            Orientation::Deg180 => assert!(a.right <= t.left && l.bottom <= n.top),
            Orientation::Deg270 => assert!(a.top >= t.bottom && l.right <= n.left),
            Orientation::Deg0 => unreachable!(),
        }
    }
}

#[test]
fn turned_and_upright_fragments_never_pair() {
    let bytes = document(|doc| {
        let f1 = type3_font(doc);
        vec![Page::new(
            dictionary! {"F1" => f1},
            &[
                upright(80.0, 740.0, "Invoice No: A-1"),
                upright(80.0, 720.0, "Date: 2026-01-01"),
                upright(300.0, 740.0, "Code: K-7"),
                // A turned label beside an upright value on its band...
                turned(DEG90, 300.0, 300.0, "Total"),
                upright(310.0, 324.0, "1000"),
                // ...and an upright label beside a turned value.
                upright(80.0, 200.0, "Name"),
                turned(DEG270, 140.0, 206.0, "Alice"),
            ],
        )]
    });
    let snapshot = snapshot(&bytes);
    let get = |text| fragment(&snapshot, 1, text);
    let mixed = [get("Total"), get("1000"), get("Name"), get("Alice")];
    for f in mixed {
        assert_eq!(f.quality, Quality::Usable, "{f:?}");
    }
    let layout = analyzed(&snapshot.fragments);
    assert_layout_consistent(&snapshot, &layout);
    let ids: Vec<&str> = mixed.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(pairs_touching(&snapshot, &ids), 0, "{:?}", layout.pairs);
    // Read without orientation, the same boxes would pair across it.
    let mut blind = snapshot.fragments.clone();
    for f in &mut blind {
        f.orientation = Orientation::Deg0;
    }
    let blind = analyzed(&blind);
    for (label, value) in [(mixed[0], mixed[1]), (mixed[2], mixed[3])] {
        assert!(
            blind.pairs.iter().any(|p| p.label.fragment_id == label.id
                && p.value.iter().any(|s| s.fragment_id == value.id)),
            "{label:?} {value:?}"
        );
    }
}

#[test]
fn turned_runs_the_script_pass_may_have_joined_stay_unknown() {
    let small = |linear: [f64; 4], x: f64, y: f64, text: &str| Run {
        size: 6,
        ..turned(linear, x, y, text)
    };
    let bytes = document(|doc| {
        let f1 = type3_font(doc);
        vec![Page::new(
            dictionary! {"F1" => f1},
            &[
                upright(80.0, 740.0, "Invoice No: A-1"),
                upright(80.0, 720.0, "Date: 2026-01-01"),
                upright(300.0, 740.0, "Code: K-7"),
                upright(300.0, 720.0, "Kind: Q"),
                // A small digit half an em up the baseline beside a turned
                // word: pdf-inspector fuses it as a superscript.
                turned(DEG90, 300.0, 300.0, "Total"),
                small(DEG90, 302.0, 304.0, "2"),
                // A small letter run there is joined as a raised script.
                turned(DEG90, 400.0, 300.0, "Notes"),
                small(DEG90, 402.0, 304.0, "ab"),
                // An ordinary turned run on the same page is held with them.
                turned(DEG90, 500.0, 300.0, "Ref: X-9"),
            ],
        )]
    });
    let snapshot = snapshot(&bytes);
    let page = on_page(&snapshot, 1);
    let fused = fragment(&snapshot, 1, "Total²");
    assert_eq!(fused.quality, Quality::Unknown, "{page:?}");
    let raised = fragment(&snapshot, 1, "ab");
    assert_eq!(raised.quality, Quality::Unknown, "{page:?}");
    assert!(has_reason(&snapshot, 1, "rotated_script_join_unverified"));
    // No other per-run reason could have held them back.
    for code in [
        "estimated_advance",
        "suspect_source_run",
        "oblique_text_present",
    ] {
        assert!(
            !has_reason(&snapshot, 1, code),
            "{:?}",
            reasons(&snapshot, 1)
        );
    }
    // Every turned run on the page is held; level runs stay usable.
    for f in page.iter().filter(|f| f.orientation != Orientation::Deg0) {
        assert_eq!(f.quality, Quality::Unknown, "{f:?}");
    }
    assert_eq!(
        fragment(&snapshot, 1, "Invoice No: A-1").quality,
        Quality::Usable
    );
    // The guard holds back the runs, not the page.
    assert_eq!(snapshot.pages[0].quality, Quality::Usable);
    assert!(snapshot.pages[0].text_available);
    let layout = analyzed(&snapshot.fragments);
    assert_eq!(pairs_touching(&snapshot, &[&fused.id, &raised.id]), 0);
    assert!(pairs_of(&layout, fragment(&snapshot, 1, "Ref: X-9")).is_empty());
    assert!(!pairs_of(&layout, fragment(&snapshot, 1, "Invoice No: A-1")).is_empty());
    // JSON names a turned fragment's orientation.
    let json = serde_json::to_value(fused).expect("a fragment serializes");
    assert_eq!(json["orientation"], "deg90");
    let names: Vec<serde_json::Value> = [
        Orientation::Deg0,
        Orientation::Deg90,
        Orientation::Deg180,
        Orientation::Deg270,
    ]
    .iter()
    .map(|o| serde_json::to_value(o).expect("an orientation serializes"))
    .collect();
    assert_eq!(names, ["deg0", "deg90", "deg180", "deg270"]);
}

/// Runs along one reading line, each word drawn on its own: `words` holds
/// each word and the gap before it in points (the first gap is ignored).
/// Every glyph of both test fonts advances one em (12 pt).
fn word_by_word(
    font: &'static str,
    linear: [f64; 4],
    (x, y): (f64, f64),
    words: &[(&str, f64)],
) -> Vec<Run> {
    let [a, b, ..] = linear;
    let mut along = 0.0;
    words
        .iter()
        .enumerate()
        .map(|(i, &(text, gap))| {
            if i > 0 {
                along += gap;
            }
            let at = (x + a * along, y + b * along);
            along += 12.0 * text.chars().count() as f64;
            composite_turned(font, linear, at.0, at.1, text)
        })
        .collect()
}

/// The three phrases drawn word by word: `Invoice No A-1` with quarter-em
/// gaps, `請求番号：INV-001` with 0 and 3 pt gaps, and `Name Alice Smith`
/// with 4 pt gaps.
const PHRASES: [&[(&str, f64)]; 3] = [
    &[("Invoice", 0.0), ("No", 3.0), ("A-1", 3.0)],
    &[("請求番号", 0.0), ("：", 0.0), ("INV-001", 3.0)],
    &[("Name", 0.0), ("Alice", 4.0), ("Smith", 4.0)],
];

/// A text matrix's linear part, the first phrase's start and the step to
/// the next phrase's start.
type PhrasePlacement = ([f64; 4], (f64, f64), (f64, f64));

/// [`PHRASES`] per page: 90°, 180°, 270°, then upright. Each page also holds
/// upright body text.
fn word_by_word_document() -> Vec<u8> {
    document(|doc| {
        let f2 = type0_font_with(doc, Object::Name(b"Identity-H".to_vec()), "請求番号：");
        let f1 = type3_font(doc);
        let placements: [PhrasePlacement; 4] = [
            (DEG90, (440.0, 150.0), (40.0, 0.0)),
            (DEG180, (560.0, 400.0), (0.0, 40.0)),
            (DEG270, (180.0, 650.0), (-40.0, 0.0)),
            ([1.0, 0.0, 0.0, 1.0], (80.0, 500.0), (0.0, -40.0)),
        ];
        placements
            .iter()
            .map(|&(linear, (x, y), (dx, dy))| {
                // Enough upright body text that pdf-inspector keeps the
                // page frame.
                let mut runs: Vec<Run> = (0..12)
                    .map(|i| upright(250.0, 760.0 - 12.0 * f64::from(i), "Body text line here"))
                    .collect();
                for (i, words) in PHRASES.iter().enumerate() {
                    let i = i as f64;
                    runs.extend(word_by_word("F2", linear, (x + dx * i, y + dy * i), words));
                }
                Page::new(dictionary! {"F1" => f1, "F2" => f2}, &runs)
            })
            .collect()
    })
}

#[test]
fn turned_words_drawn_one_by_one_never_pair_with_each_other() {
    let snapshot = snapshot(&word_by_word_document());
    let layout = analyzed(&snapshot.fragments);
    assert_layout_consistent(&snapshot, &layout);
    let words: Vec<&str> = PHRASES.iter().flat_map(|p| p.iter().map(|w| w.0)).collect();
    let orientations = [Orientation::Deg90, Orientation::Deg180, Orientation::Deg270];
    for (page, orientation) in (1..).zip(orientations) {
        assert_eq!(
            snapshot.pages[page as usize - 1].quality,
            Quality::Usable,
            "{:?} {:?}",
            snapshot.pages[page as usize - 1],
            on_page(&snapshot, page)
        );
        let pieces: Vec<&SourceFragment> =
            words.iter().map(|w| fragment(&snapshot, page, w)).collect();
        for f in &pieces {
            // Never merged upstream, and usable on their own.
            assert_eq!(f.quality, Quality::Usable, "{f:?}");
            assert_eq!(f.orientation, orientation, "{f:?}");
        }
        let ids: Vec<&str> = pieces.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            pairs_touching(&snapshot, &ids),
            0,
            "{orientation:?}: {:?}",
            layout
                .pairs
                .iter()
                .filter(|p| p.page == page)
                .collect::<Vec<_>>()
        );
        // The pieces still form their reading lines.
        for words in PHRASES {
            let first = fragment(&snapshot, page, words[0].0);
            let index = snapshot.fragments.iter().position(|f| f.id == first.id);
            let line = layout
                .lines
                .iter()
                .find(|l| index.is_some_and(|i| l.fragments.contains(&i)))
                .expect("a held piece stays on its line");
            assert_eq!(line.fragments.len(), words.len(), "{line:?}");
        }
    }
    // Upright, pdf-inspector merges the same words into one run per phrase,
    // so the turned pieces are held where an upright page reads one phrase.
    let upright: Vec<&str> = on_page(&snapshot, 4)
        .iter()
        .map(|f| f.raw_text.as_str())
        .collect();
    for phrase in ["Invoice No A-1", "Name Alice Smith"] {
        assert!(upright.contains(&phrase), "{upright:?}");
    }
    let japanese = upright
        .iter()
        .find(|t| t.starts_with("請求番号"))
        .expect("the Japanese phrase is one run");
    assert!(japanese.ends_with("INV-001"), "{japanese:?}");
    assert_eq!(
        signature(&snapshot, 4)
            .into_iter()
            .filter(|(label, ..)| label.starts_with("請求番号"))
            .collect::<Vec<_>>(),
        [("請求番号".into(), vec!["INV-001".into()], Placement::Inline)]
    );
}

/// A script hung on a script: pdf-inspector measures the outer chain's shift
/// from the body baseline, where it sits, so it reports no shift and no
/// script digits.
#[test]
fn a_script_chain_hung_on_a_turned_script_is_held_with_its_page() {
    let sized = |size: i64, x: f64, y: f64, text: &str| Run {
        size,
        ..turned(DEG90, x, y, text)
    };
    let bytes = document(|doc| {
        let f1 = type3_font(doc);
        let mut runs: Vec<Run> = (0..8)
            .map(|i| upright(80.0, 760.0 - 14.0 * f64::from(i), "Body text line here"))
            .collect();
        // A 90° run's box lies one em left of its origin and climbs from it;
        // the script pass reads that box's x as the reading axis.
        runs.extend([
            sized(12, 300.0, 300.0, "Anchor"),
            // Raised 2 pt off `Anchor`, overlapping its right edge.
            sized(7, 305.0, 302.0, "Q"),
            // On `Anchor`'s baseline, touching `Q`'s right edge.
            sized(4, 309.0, 300.0, "XY"),
            sized(4, 313.0, 300.0, "ZW"),
            turned(DEG90, 500.0, 300.0, "Ref: X-9"),
        ]);
        vec![Page::new(dictionary! {"F1" => f1}, &runs)]
    });
    let snapshot = snapshot(&bytes);
    let page = on_page(&snapshot, 1);
    let chain = fragment(&snapshot, 1, "XYZW");
    assert_eq!(chain.orientation, Orientation::Deg90, "{page:?}");
    assert_eq!(chain.quality, Quality::Unknown, "{page:?}");
    assert_eq!(fragment(&snapshot, 1, "Q").quality, Quality::Unknown);
    assert!(has_reason(&snapshot, 1, "rotated_script_join_unverified"));
    for f in page.iter().filter(|f| f.orientation != Orientation::Deg0) {
        assert_eq!(f.quality, Quality::Unknown, "{f:?}");
    }
    let turned: Vec<&str> = page
        .iter()
        .filter(|f| f.orientation != Orientation::Deg0)
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(pairs_touching(&snapshot, &turned), 0);
}

/// Per page, one turned orientation: `Name:` with `Alice` 5 em further
/// along its line, and on the next line `Ref:` with `X-9` 2 em further and
/// `Tokyo` 5 em after that, beside upright body text. Glyphs advance one em
/// (12 pt).
fn strict_rules_document() -> Vec<u8> {
    document(|doc| {
        let f1 = type3_font(doc);
        let pages = [
            (DEG90, (300.0, 200.0)),
            (DEG180, (500.0, 300.0)),
            (DEG270, (300.0, 600.0)),
        ];
        pages
            .into_iter()
            .map(|(linear, (x, y))| {
                let [a, b, c, d] = linear;
                // Along the reading line `along` pt, `line` lines down.
                let at = |line: f64, along: f64, text| {
                    let (x, y) = (
                        x + a * along - c * 40.0 * line,
                        y + b * along - d * 40.0 * line,
                    );
                    turned(linear, x, y, text)
                };
                let mut runs = vec![
                    upright(80.0, 760.0, "Invoice No: A-1"),
                    upright(80.0, 740.0, "Total"),
                    upright(200.0, 740.0, "1000"),
                    upright(80.0, 720.0, "Date: 2026-01-01"),
                ];
                runs.extend([
                    at(0.0, 0.0, "Name:"),
                    at(0.0, 5.0 * 12.0 + 60.0, "Alice"),
                    at(1.0, 0.0, "Ref:"),
                    at(1.0, 4.0 * 12.0 + 24.0, "X-9"),
                    at(1.0, 7.0 * 12.0 + 24.0 + 60.0, "Tokyo"),
                ]);
                Page::new(dictionary! {"F1" => f1}, &runs)
            })
            .collect()
    })
}

#[test]
fn turned_runs_keep_the_delimiter_gap_and_split_rules() {
    let snapshot = snapshot(&strict_rules_document());
    let layout = analyzed(&snapshot.fragments);
    assert_layout_consistent(&snapshot, &layout);
    for (page, orientation) in [
        (1, Orientation::Deg90),
        (2, Orientation::Deg180),
        (3, Orientation::Deg270),
    ] {
        let get = |text| fragment(&snapshot, page, text);
        for text in ["Name:", "Alice", "Ref:", "X-9", "Tokyo"] {
            let f = get(text);
            assert_eq!(f.quality, Quality::Usable, "{f:?}");
            assert_eq!(f.orientation, orientation, "{f:?}");
        }
        let right = |label: &SourceFragment| {
            let pairs: Vec<&Pair> = layout
                .pairs
                .iter()
                .filter(|p| p.label.fragment_id == label.id && p.placement == Placement::Right)
                .collect();
            assert_eq!(pairs.len(), 1, "{orientation:?}: {pairs:?}");
            pairs[0]
        };
        // 5 em from `Name:`, beyond MAX_DELIMITER_GAP_EM: position only.
        let name = right(get("Name:"));
        assert_eq!(span_text(&snapshot, &name.value[0]), "Alice");
        assert_eq!(name.binding, Binding::Geometric, "{orientation:?}");
        assert_eq!(name.split, [false], "{orientation:?}");
        // 2 em from `Ref:`: delimiter-bound, but `Tokyo` follows on the line.
        let reference = right(get("Ref:"));
        assert_eq!(span_text(&snapshot, &reference.value[0]), "X-9");
        assert_eq!(reference.binding, Binding::Delimiter, "{orientation:?}");
        assert_eq!(reference.split, [true], "{orientation:?}");
    }
}
