//! Deterministic synthetic PDFs with arbitrary Unicode text and no font file.
//!
//! Each distinct character gets a sequential CID in one Type0 font with the
//! `Identity-H` encoding. Its CIDFontType2 descendant declares widths (`/W`)
//! but embeds no glyph program, and a ToUnicode CMap maps every CID back to
//! its code point. Text extraction needs only the CIDs, the widths and the
//! ToUnicode map, so Japanese text can be built without any font file;
//! renderers would substitute a font (glyph fidelity is not part of this set).
//! Sequential CIDs (not CID = code point) make extraction depend on the
//! ToUnicode map rather than on an identity guess.
//!
//! Observed with pdf-inspector 1.25.2: each run becomes one text item with
//! its exact text, a box from the baseline up one em and its declared
//! advance. Runs on one line whose gap is at most about 0.5 em are joined
//! into one item with a space; gaps of 0.75 em or more stay separate. Cases
//! that need separate runs therefore keep gaps of at least one em.
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, Stream, StringFormat, dictionary};
use std::collections::BTreeMap;

/// A4 portrait in PDF points.
pub const PAGE_WIDTH: f64 = 595.0;
pub const PAGE_HEIGHT: f64 = 842.0;

/// One text-showing operation (`Tj`) at a position on a page.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    /// 1-based page number.
    #[serde(default = "first_page")]
    pub page: u32,
    /// Left edge of the run in points from the page's left edge.
    #[serde(default)]
    pub x: Option<f64>,
    /// Right edge instead of `x`, for right-aligned columns such as amounts.
    #[serde(default)]
    pub right: Option<f64>,
    /// Baseline in points from the page's top edge.
    pub y: f64,
    /// Font size in points.
    #[serde(default = "default_size")]
    pub size: f64,
    pub text: String,
}

impl Run {
    /// Left edge in points; exactly one of `x` and `right` must be set.
    pub fn left(&self) -> f64 {
        match (self.x, self.right) {
            (Some(x), None) => x,
            (None, Some(right)) => right - text_width(&self.text, self.size),
            _ => panic!("run {:?} needs exactly one of x and right", self.text),
        }
    }
}

fn first_page() -> u32 {
    1
}

fn default_size() -> f64 {
    10.0
}

/// Advance in thousandths of an em: 1000 for wide East Asian characters,
/// 500 otherwise (the same classes as the layout estimator in `vf-source`).
pub fn advance(c: char) -> u32 {
    let wide = matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6);
    if wide { 1000 } else { 500 }
}

/// Width of `text` in points at `size`.
pub fn text_width(text: &str, size: f64) -> f64 {
    text.chars().map(|c| f64::from(advance(c))).sum::<f64>() * size / 1000.0
}

/// Build a PDF whose pages hold `runs`, one `Tj` per run, in the given order.
pub fn build(runs: &[Run]) -> Vec<u8> {
    let pages = runs.iter().map(|r| r.page).max().unwrap_or(1);
    assert!(
        runs.iter().all(|r| r.page >= 1),
        "pages are 1-based in case definitions"
    );
    // Sequential CIDs from 1 in code point order; CID 0 stays .notdef.
    let mut cids: BTreeMap<char, u16> = BTreeMap::new();
    for c in runs.iter().flat_map(|r| r.text.chars()) {
        assert!(
            (c as u32) <= 0xFFFF && !c.is_control(),
            "builder covers printable BMP characters only: {c:?}"
        );
        cids.entry(c).or_insert(0);
    }
    for (n, cid) in cids.values_mut().enumerate() {
        *cid = u16::try_from(n + 1).expect("character set fits in CIDs");
    }

    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = add_font(&mut doc, &cids);

    let mut kids = Vec::new();
    for page in 1..=pages {
        let mut operations = Vec::new();
        for run in runs.iter().filter(|r| r.page == page) {
            let code: Vec<u8> = run
                .text
                .chars()
                .flat_map(|c| cids[&c].to_be_bytes())
                .collect();
            operations.extend([
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), Object::Real(run.size as f32)]),
                Operation::new(
                    "Td",
                    vec![
                        Object::Real(run.left() as f32),
                        Object::Real((PAGE_HEIGHT - run.y) as f32),
                    ],
                ),
                Operation::new("Tj", vec![Object::String(code, StringFormat::Hexadecimal)]),
                Operation::new("ET", vec![]),
            ]);
        }
        let content = Content { operations }
            .encode()
            .expect("synthetic text operations must encode");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(PAGE_WIDTH as f32), Object::Real(PAGE_HEIGHT as f32)],
            "Resources" => dictionary! {"Font" => dictionary! {"F1" => font_id}},
        });
        kids.push(Object::Reference(page_id));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Count" => i64::from(pages), "Kids" => kids,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .expect("synthetic PDF must serialize");
    bytes
}

fn add_font(doc: &mut Document, cids: &BTreeMap<char, u16>) -> lopdf::ObjectId {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(&char, &u16)> = cids.iter().collect();
    // CMap sections hold at most 100 entries each.
    for chunk in entries.chunks(100) {
        cmap += &format!("{} beginbfchar\n", chunk.len());
        for (c, cid) in chunk {
            cmap += &format!("<{cid:04X}> <{:04X}>\n", **c as u32);
        }
        cmap += "endbfchar\n";
    }
    cmap += "endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n";
    let to_unicode = doc.add_object(Stream::new(dictionary! {}, cmap.into_bytes()));

    // CIDs are sequential, so one `/W` entry lists every width in CID order.
    let mut by_cid: Vec<(u16, char)> = cids.iter().map(|(c, cid)| (*cid, *c)).collect();
    by_cid.sort_unstable();
    let widths: Vec<Object> = by_cid
        .iter()
        .map(|(_, c)| Object::Integer(i64::from(advance(*c))))
        .collect();
    let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "VoltfillEvalSans", "Flags" => 4,
        "FontBBox" => vec![0.into(), Object::Integer(-120), 1000.into(), 880.into()],
        "ItalicAngle" => 0, "Ascent" => 880, "Descent" => -120, "CapHeight" => 700,
        "StemV" => 80,
    });
    let cid_font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "VoltfillEvalSans",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor, "DW" => 1000,
        "W" => vec![1.into(), Object::Array(widths)],
        "CIDToGIDMap" => "Identity",
    });
    let font: Dictionary = dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "VoltfillEvalSans",
        "Encoding" => "Identity-H", "DescendantFonts" => vec![Object::Reference(cid_font)],
        "ToUnicode" => to_unicode,
    };
    doc.add_object(font)
}
